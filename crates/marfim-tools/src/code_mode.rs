//! W5.9 CodeMode (distilled: opencode `packages/codemode`, clean-room):
//! one opt-in `Code` tool collapses N tool schemas into a single code
//! surface. The model writes a short rhai script whose `tool_call(name,
//! args_json)` invocations DESCRIBE the calls it wants; the interpreter
//! is restricted (no fs/network/process, an op cap, a call cap, and a
//! string-size cap) and the collected plan executes through the REAL
//! tool host — hooks → permission engine → sandbox → bounding — exactly
//! as if each call had been made directly. CodeMode is therefore a
//! BATCHING/COMPOSITION surface, never a permission bypass.
//!
//! Plan-then-execute (not inline execution) is the honest shape for a
//! sync-capable tool: results come back in one result object and the
//! next Code call can compose over them.

use crate::registry::{ToolCtx, ToolOutput};
use rhai::{Engine, OptimizationLevel};
use serde_json::Value;

/// The interpreter's budget: rhai `max_operations` (loop/expr steps).
pub const MAX_OPS: u64 = 20_000;
/// Tool-call requests a single script may make.
pub const MAX_CALLS: usize = 16;
/// Script size cap (bytes) — a code surface, not a data channel.
pub const MAX_SCRIPT: usize = 16 * 1024;

pub fn code(_ctx: &ToolCtx, _args: &Value) -> ToolOutput {
    ToolOutput::err("Code must be dispatched through the tool host (plan execution)")
}

/// The plan executor: runs one inner call through the full host path.
pub type PlanHost<'a> = dyn Fn(
        String,
        Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (String, bool)> + Send + 'a>>
    + Send
    + Sync
    + 'a;

/// The host arm: evaluate + execute the plan. Returns a JSON object
/// `{result, calls:[{tool,args,output,is_error}]}`.
pub async fn code_exec<'a>(_ctx: &ToolCtx, host: &'a PlanHost<'a>, args: &Value) -> ToolOutput {
    let Some(script) = args["code"].as_str() else {
        return ToolOutput::err("missing `code`");
    };
    if script.len() > MAX_SCRIPT {
        return ToolOutput::err(format!("script exceeds {MAX_SCRIPT} bytes cap"));
    }
    // The rhai Engine holds Rc internals — NOT Send. The eval phase is
    // scoped so the engine dies BEFORE the first await (the plan
    // execution phase), keeping code_exec's future Send.
    let (result, plan) = {
        let mut engine = Engine::new();
        engine.set_optimization_level(OptimizationLevel::Simple);
        engine.set_max_operations(MAX_OPS);
        engine.set_max_string_size(1_000_000);
        engine.set_max_array_size(10_000);
        engine.set_max_expr_depths(32, 32);

        let calls: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        engine
            .register_fn("tool_call", move |name: &str, args_json: &str| {
                let idx;
                {
                    let mut c = calls_clone.lock().unwrap_or_else(|p| p.into_inner());
                    if c.len() >= MAX_CALLS {
                        return format!("{{\"error\":\"call cap reached ({MAX_CALLS})\"}}");
                    }
                    let parsed: Value = serde_json::from_str(args_json)
                        .unwrap_or(Value::String(args_json.to_string()));
                    c.push((name.to_string(), parsed));
                    idx = c.len() - 1;
                }
                format!("{{\"pending\":true,\"index\":{idx}}}")
            })
            .register_fn("log", |v: rhai::Dynamic| format!("{v}"));

        let result = match engine.eval::<rhai::Dynamic>(script) {
            Ok(v) => v.to_string(),
            Err(e) => return ToolOutput::err(format!("script error: {e} (op cap {MAX_OPS})")),
        };

        let plan = calls.lock().unwrap_or_else(|p| p.into_inner()).clone();
        (result, plan)
    };
    let mut executed = Vec::new();
    for (name, call_args) in &plan {
        let (out, is_err) = host(name.clone(), call_args.clone()).await;
        executed.push(serde_json::json!({
            "tool": name,
            "args": call_args,
            "output": out,
            "is_error": is_err,
        }));
        if is_err {
            // A failed call stops the plan: the model sees exactly how far
            // it got and why (fail-visible, never a partial lie).
            break;
        }
    }
    let v = serde_json::json!({
        "result": result,
        "calls": executed,
        "planned": plan.len(),
        "executed": executed.len(),
    });
    ToolOutput::ok(serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolCtx;
    use std::future::Future;
    use std::pin::Pin;

    fn noop_host(
        name: String,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = (String, bool)> + Send>> {
        let out = format!("{name} ran with {args}");
        Box::pin(async move { (out, false) })
    }

    #[tokio::test]
    async fn script_result_and_calls_flow_through() {
        let ctx = ToolCtx::default();
        let out = code_exec(
            &ctx,
            &noop_host,
            &serde_json::json!({
                "code": "let a = 40; let b = 2; tool_call(\"Think\", \"sum \" + (a + b).to_string()); a * b"
            }),
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("\"result\": \"80\""), "{}", out.output);
        assert!(out.output.contains("Think"), "{}", out.output);
        assert!(out.output.contains("sum 42"), "{}", out.output);
    }

    #[tokio::test]
    async fn call_cap_and_op_cap_refuse_honestly() {
        let ctx = ToolCtx::default();
        // op cap: an endless loop dies at MAX_OPS with a script error
        let out = code_exec(
            &ctx,
            &noop_host,
            &serde_json::json!({"code": "let mut i = 0; loop { i += 1; }"}),
        )
        .await;
        assert!(out.is_error, "runaway loop must die: {}", out.output);
        assert!(out.output.contains("script error"), "{}", out.output);
        // call cap: the 17th request is refused inside the script
        let mut script = String::new();
        for i in 0..(MAX_CALLS + 2) {
            script.push_str(&format!("tool_call(\"Think\", \"{i}\");\n"));
        }
        let out = code_exec(&ctx, &noop_host, &serde_json::json!({"code": script})).await;
        assert!(!out.is_error, "{}", out.output);
        assert!(
            out.output.contains(&format!("\"planned\": {MAX_CALLS}")),
            "{}",
            out.output
        );
    }

    #[tokio::test]
    async fn failing_call_stops_the_plan_fail_visible() {
        let ctx = ToolCtx::default();
        let host =
            |name: String, _args: Value| -> Pin<Box<dyn Future<Output = (String, bool)> + Send>> {
                let out = if name == "Boom" {
                    ("exploded".to_string(), true)
                } else {
                    ("ok".to_string(), false)
                };
                Box::pin(async move { out })
            };
        let out = code_exec(
            &ctx,
            &host,
            &serde_json::json!({
                "code": "tool_call(\"Think\", \"fine\"); tool_call(\"Boom\", \"{}\"); tool_call(\"Think\", \"never\")"
            }),
        )
        .await;
        assert!(out.output.contains("exploded"), "{}", out.output);
        assert!(
            !out.output.contains("never"),
            "calls after a failure never run: {}",
            out.output
        );
        assert!(out.output.contains("\"executed\": 2"), "{}", out.output);
    }

    #[tokio::test]
    async fn script_size_and_missing_code_refuse() {
        let ctx = ToolCtx::default();
        let big = "x".repeat(MAX_SCRIPT + 1);
        assert!(
            code_exec(&ctx, &noop_host, &serde_json::json!({"code": big}))
                .await
                .is_error
        );
        assert!(
            code_exec(&ctx, &noop_host, &serde_json::json!({}))
                .await
                .is_error
        );
        assert!(
            code(&ctx, &serde_json::json!({"code": "1"})).is_error,
            "direct run refuses honestly"
        );
    }
}
