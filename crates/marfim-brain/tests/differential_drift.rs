//! Differential tests (P4-A): replay the oracle-recorded JSONL fixtures for
//! the drift/calibration cluster — `fixtures/intel/{adwin,conformal,abers,
//! conservative,ensemble}.jsonl` — against the Rust ports. Each fixture
//! line is `{"input": ..., "output": ...}` where the output was produced by
//! RUNNING the Node oracle (scripts/fixture-*.js). Ints and strings must be
//! byte-exact; floats are compared at <=1-ulp tolerance
//! (`marfim_brain::util::ulp_close`) — the documented allowance for the
//! oracle's transcendental `Math.log`/`Math.exp`/`Math.pow` call sites.
//! Error paths are recorded as `{"error": "<message>"}` and asserted as
//! exact `Err` strings.

use marfim_brain::intel::{abers, adwin, conformal, conservative, ensemble};
use marfim_brain::util::ulp_close;
use serde_json::{json, Value};

fn fixture_lines(name: &str) -> Vec<(usize, Value, Value)> {
    let path = format!(
        "{}/../../fixtures/intel/{name}.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {name}: {e} (path {path})"));
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{name}:{i}: {e}"));
            (i + 1, v["input"].clone(), v["output"].clone())
        })
        .collect()
}

/// Recursive comparator: byte-exact ints/strings/bools/nulls, <=1-ulp
/// floats (int-vs-float spellings of the same value are equal values).
fn compare(a: &Value, b: &Value, path: &str, errs: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            for k in ma.keys() {
                if !mb.contains_key(k) {
                    errs.push(format!("{path}: key {k:?} missing in rust output"));
                }
            }
            for k in mb.keys() {
                if !ma.contains_key(k) {
                    errs.push(format!("{path}: extra key {k:?} in rust output"));
                }
            }
            for (k, va) in ma {
                if let Some(vb) = mb.get(k) {
                    compare(va, vb, &format!("{path}.{k}"), errs);
                }
            }
        }
        (Value::Array(aa), Value::Array(ab)) => {
            if aa.len() != ab.len() {
                errs.push(format!("{path}: length {} != {}", aa.len(), ab.len()));
                return;
            }
            for (i, (va, vb)) in aa.iter().zip(ab.iter()).enumerate() {
                compare(va, vb, &format!("{path}[{i}]"), errs);
            }
        }
        (Value::String(x), Value::String(y)) => {
            if x != y {
                errs.push(format!("{path}: string {x:?} != {y:?}"));
            }
        }
        (Value::Bool(x), Value::Bool(y)) => {
            if x != y {
                errs.push(format!("{path}: bool {x} != {y}"));
            }
        }
        (Value::Null, Value::Null) => {}
        (Value::Number(x), Value::Number(y)) => {
            let eq = match (x.as_i64(), y.as_i64()) {
                (Some(ix), Some(iy)) => ix == iy,
                _ => match (x.as_u64(), y.as_u64()) {
                    (Some(ux), Some(uy)) => ux == uy,
                    _ => match (x.as_f64(), y.as_f64()) {
                        (Some(fx), Some(fy)) => ulp_close(fx, fy),
                        _ => false,
                    },
                },
            };
            if !eq {
                errs.push(format!("{path}: number {x} !~ {y}"));
            }
        }
        _ => errs.push(format!("{path}: type mismatch {a} vs {b}")),
    }
}

fn opt_f64(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

// ---------------------------------------------------------------- adwin ---

fn run_adwin_op(input: &Value) -> Result<Value, String> {
    match input.get("op").and_then(Value::as_str).unwrap_or("") {
        "adwin.update" => {
            let mut a = adwin::ADWIN::new(
                input["delta"].as_f64().unwrap_or(0.002),
                input["maxBuckets"].as_i64().unwrap_or(5),
                input["minWidth"].as_i64().unwrap_or(32),
            )?;
            let stream = input["stream"].as_array().cloned().unwrap_or_default();
            let mut updates = Vec::new();
            for v in &stream {
                updates.push(a.update(v.as_bool().unwrap_or(false)));
            }
            Ok(json!({ "updates": updates, "status": a.status(), "dict": a.to_json() }))
        }
        "adwin.reset_then_update" => {
            let mut a = adwin::ADWIN::default();
            a.reset();
            let stream = input["stream"].as_array().cloned().unwrap_or_default();
            let mut updates = Vec::new();
            for v in &stream {
                updates.push(a.update(v.as_bool().unwrap_or(false)));
            }
            Ok(json!({ "updates": updates, "status": a.status(), "dict": a.to_json() }))
        }
        "adwin.from_dict_then_update" => {
            let mut a = adwin::ADWIN::default();
            a.from_json(&input["dict"]);
            let more = input["more"].as_array().cloned().unwrap_or_default();
            let mut updates = Vec::new();
            for v in &more {
                updates.push(a.update(v.as_bool().unwrap_or(false)));
            }
            Ok(json!({ "updates": updates, "status": a.status(), "dict": a.to_json() }))
        }
        "adwin.ctor_error" => adwin::ADWIN::new(
            input["delta"].as_f64().unwrap_or(0.002),
            input["maxBuckets"].as_i64().unwrap_or(5),
            input["minWidth"].as_i64().unwrap_or(32),
        )
        .map(|_| json!("ok")),
        "adwin.consensus" => Ok(adwin::consensus(&input["ph"], &input["adwin"])),
        "adwin.dc_update" => {
            let ph_opts = &input["ph"];
            let ad_opts = &input["ad"];
            let ph = conformal::PageHinkleyDrift::with_options(
                ph_opts
                    .get("threshold")
                    .and_then(Value::as_f64)
                    .unwrap_or(8.0),
                ph_opts.get("alpha").and_then(Value::as_f64).unwrap_or(0.98),
                ph_opts.get("delta").and_then(Value::as_f64).unwrap_or(0.01),
                ph_opts.get("warmup").and_then(Value::as_i64).unwrap_or(25),
            );
            let detector = adwin::ADWIN::new(
                ad_opts
                    .get("delta")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.002),
                ad_opts
                    .get("maxBuckets")
                    .and_then(Value::as_i64)
                    .unwrap_or(5),
                ad_opts
                    .get("minWidth")
                    .and_then(Value::as_i64)
                    .unwrap_or(32),
            )?;
            let mut dc = adwin::DriftConsensus::with_detectors(ph, detector);
            let stream = input["stream"].as_array().cloned().unwrap_or_default();
            let mut updates = Vec::new();
            for v in &stream {
                updates.push(dc.update(v.as_bool().unwrap_or(false)));
            }
            Ok(json!({ "updates": updates, "status": dc.status(), "dict": dc.to_json() }))
        }
        "adwin.dc_roundtrip" => {
            let mut dc = adwin::DriftConsensus::default();
            dc.from_json(&input["dict"]);
            Ok(json!({ "status": dc.status(), "dict": dc.to_json() }))
        }
        "adwin.bocpd" => {
            let series = f64s_of(&input["series"]);
            adwin::bocpd(
                &series,
                input.get("hazard").and_then(Value::as_f64).unwrap_or(100.0),
                opt_f64(input, "variance"),
                opt_f64(input, "priorMean"),
            )
        }
        "adwin.halves" => {
            let labels = f64s_of(&input["labels"]);
            Ok(adwin::halves_drift(&labels))
        }
        other => Err(format!("unknown adwin op {other:?}")),
    }
}

fn f64s_of(v: &Value) -> Vec<f64> {
    v.as_array()
        .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
        .unwrap_or_default()
}

#[test]
fn adwin_fixtures_replay() {
    let cases = fixture_lines("adwin");
    assert!(cases.len() >= 40, "expected the full adwin fixture set");
    for (no, input, output) in cases {
        let label = input
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| input.get("op").and_then(Value::as_str))
            .unwrap_or("?")
            .to_string();
        match (
            output.get("error").and_then(Value::as_str),
            run_adwin_op(&input),
        ) {
            (Some(msg), Err(e)) => assert_eq!(e, msg, "{label} (line {no})"),
            (Some(msg), Ok(v)) => panic!("{label} (line {no}): expected Err({msg}), got {v}"),
            (None, Err(e)) => panic!("{label} (line {no}): unexpected Err({e})"),
            (None, Ok(v)) => {
                let mut errs = Vec::new();
                compare(&v, &output, "$", &mut errs);
                assert!(
                    errs.is_empty(),
                    "{label} (line {no}) diverged:\n{}",
                    errs.join("\n")
                );
            }
        }
    }
}

// ------------------------------------------------------------ conformal ---

fn run_conformal_op(input: &Value) -> Result<Value, String> {
    match input.get("op").and_then(Value::as_str).unwrap_or("") {
        "conformal.cal_run" => {
            let mut c = conformal::ConformalCalibrator::with_options(
                input["alpha"].as_f64().unwrap_or(0.1),
                input["maxHistory"].as_u64().unwrap_or(400) as usize,
            )?;
            let obs = input["obs"].as_array().cloned().unwrap_or_default();
            for o in &obs {
                let score = o.get(0).and_then(Value::as_f64).unwrap_or(0.0);
                let outcome = o.get(1).and_then(Value::as_str).unwrap_or("");
                c.observe(score, outcome);
            }
            let probe = input["probe"].as_f64().unwrap_or(0.0);
            let probe_alpha = opt_f64(input, "probeAlpha");
            Ok(json!({
                "threshold": c.threshold(None),
                "thresholdOverride": c.threshold(probe_alpha),
                "verdict": c.verdict(probe, None),
                "verdictOverride": c.verdict(probe, probe_alpha),
                "empirical": c.empirical_accuracy(0.0),
                "empiricalMin": c.empirical_accuracy(0.5),
                "dict": c.to_json(),
            }))
        }
        "conformal.cal_ctor_error" => conformal::ConformalCalibrator::with_options(
            input["alpha"].as_f64().unwrap_or(0.1),
            input["maxHistory"].as_u64().unwrap_or(400) as usize,
        )
        .map(|_| json!("ok")),
        "conformal.cal_defaults" => {
            let c = conformal::ConformalCalibrator::new()?;
            Ok(json!({ "threshold": c.threshold(None), "dict": c.to_json() }))
        }
        "conformal.cal_roundtrip" => {
            let mut c = conformal::ConformalCalibrator::new()?;
            c.from_json(&input["dict"]);
            let probe = input["probe"].as_f64().unwrap_or(0.0);
            let probe_alpha = opt_f64(input, "probeAlpha");
            Ok(json!({
                "threshold": c.threshold(None),
                "verdict": c.verdict(probe, probe_alpha),
                "empirical": c.empirical_accuracy(0.0),
                "dict": c.to_json(),
            }))
        }
        "conformal.qbc" => {
            let candidates = input["candidates"].as_array().cloned().unwrap_or_default();
            let votes = input["votes"].as_array().cloned().unwrap_or_default();
            let rows = conformal::query_by_committee(
                &candidates,
                &votes,
                input["topK"].as_u64().unwrap_or(3) as usize,
            )?;
            Ok(Value::Array(rows))
        }
        "conformal.ph_update" => {
            let mut p = conformal::PageHinkleyDrift::with_options(
                input["threshold"].as_f64().unwrap_or(8.0),
                input["alpha"].as_f64().unwrap_or(0.98),
                input["delta"].as_f64().unwrap_or(0.01),
                input["warmup"].as_i64().unwrap_or(25),
            );
            let stream = input["stream"].as_array().cloned().unwrap_or_default();
            let mut updates = Vec::new();
            for v in &stream {
                updates.push(p.update(v.as_bool().unwrap_or(false)));
            }
            Ok(json!({ "updates": updates, "status": p.status(), "dict": p.to_json() }))
        }
        "conformal.ph_reset" => {
            let mut p = conformal::PageHinkleyDrift::default();
            p.reset();
            let stream = input["stream"].as_array().cloned().unwrap_or_default();
            let mut updates = Vec::new();
            for v in &stream {
                updates.push(p.update(v.as_bool().unwrap_or(false)));
            }
            Ok(json!({ "updates": updates, "status": p.status(), "dict": p.to_json() }))
        }
        "conformal.ph_roundtrip" => {
            let mut p = conformal::PageHinkleyDrift::default();
            p.from_json(&input["dict"]);
            Ok(json!({ "status": p.status(), "dict": p.to_json() }))
        }
        other => Err(format!("unknown conformal op {other:?}")),
    }
}

#[test]
fn conformal_fixtures_replay() {
    let cases = fixture_lines("conformal");
    assert!(cases.len() >= 25, "expected the full conformal fixture set");
    for (no, input, output) in cases {
        let label = input
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| input.get("op").and_then(Value::as_str))
            .unwrap_or("?")
            .to_string();
        match (
            output.get("error").and_then(Value::as_str),
            run_conformal_op(&input),
        ) {
            (Some(msg), Err(e)) => assert_eq!(e, msg, "{label} (line {no})"),
            (Some(msg), Ok(v)) => panic!("{label} (line {no}): expected Err({msg}), got {v}"),
            (None, Err(e)) => panic!("{label} (line {no}): unexpected Err({e})"),
            (None, Ok(v)) => {
                let mut errs = Vec::new();
                compare(&v, &output, "$", &mut errs);
                assert!(
                    errs.is_empty(),
                    "{label} (line {no}) diverged:\n{}",
                    errs.join("\n")
                );
            }
        }
    }
}

// ---------------------------------------------------------------- abers ---

fn pairs_of(v: &Value) -> Vec<[f64; 2]> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| {
                    [
                        p.get(0).and_then(Value::as_f64).unwrap_or(0.0),
                        p.get(1).and_then(Value::as_f64).unwrap_or(0.0),
                    ]
                })
                .collect()
        })
        .unwrap_or_default()
}

fn run_abers_op(input: &Value) -> Result<Value, String> {
    match input.get("op").and_then(Value::as_str).unwrap_or("") {
        "abers.isotonic_fit" => {
            let pairs = pairs_of(&input["pairs"]);
            let weights = input
                .get("weights")
                .and_then(Value::as_array)
                .map(|a| f64s_of(&Value::Array(a.clone())));
            let steps = abers::isotonic_fit(&pairs, weights.as_deref())?;
            Ok(json!(steps))
        }
        "abers.isotonic_predict" => {
            let steps = pairs_of(&input["steps"]);
            let x = input["x"].as_f64().unwrap_or(0.0);
            Ok(json!(abers::isotonic_predict(&steps, x)))
        }
        "abers.venn_abers" => {
            let calib = pairs_of(&input["calib"]);
            let test_p = input["testP"].as_f64().unwrap_or(0.0);
            let [lo, hi, mid] = abers::venn_abers(&calib, test_p);
            Ok(json!([lo, hi, mid]))
        }
        "abers.wcq" => {
            let scores = f64s_of(&input["scores"]);
            let weights = input
                .get("weights")
                .and_then(Value::as_array)
                .map(|a| f64s_of(&Value::Array(a.clone())));
            let alpha = input["alpha"].as_f64().unwrap_or(0.1);
            let q = abers::weighted_conformal_quantile(&scores, weights.as_deref(), alpha)?;
            Ok(json!(q))
        }
        "abers.ece" => {
            let scores = f64s_of(&input["scores"]);
            let outcomes = f64s_of(&input["outcomes"]);
            let bins = input["bins"].as_u64().unwrap_or(5) as usize;
            Ok(json!(abers::ece(&scores, &outcomes, bins)))
        }
        "abers.report" => {
            let rows = pairs_of(&input["rows"]);
            let folds = input["folds"].as_u64().unwrap_or(4) as usize;
            let bins = input["bins"].as_u64().unwrap_or(5) as usize;
            Ok(abers::ece_report(&rows, folds, bins))
        }
        other => Err(format!("unknown abers op {other:?}")),
    }
}

#[test]
fn abers_fixtures_replay() {
    let cases = fixture_lines("abers");
    assert!(cases.len() >= 40, "expected the full abers fixture set");
    for (no, input, output) in cases {
        let label = input
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| input.get("op").and_then(Value::as_str))
            .unwrap_or("?")
            .to_string();
        match (
            output.get("error").and_then(Value::as_str),
            run_abers_op(&input),
        ) {
            (Some(msg), Err(e)) => assert_eq!(e, msg, "{label} (line {no})"),
            (Some(msg), Ok(v)) => panic!("{label} (line {no}): expected Err({msg}), got {v}"),
            (None, Err(e)) => panic!("{label} (line {no}): unexpected Err({e})"),
            (None, Ok(v)) => {
                let mut errs = Vec::new();
                compare(&v, &output, "$", &mut errs);
                assert!(
                    errs.is_empty(),
                    "{label} (line {no}) diverged:\n{}",
                    errs.join("\n")
                );
            }
        }
    }
}

// ---------------------------------------------------------- conservative ---

fn run_conservative_op(input: &Value) -> Result<Value, String> {
    match input.get("op").and_then(Value::as_str).unwrap_or("") {
        "cons.radius" => {
            let r = conservative::hoeffding_radius(
                input["n"].as_i64().unwrap_or(1),
                input["delta"].as_f64().unwrap_or(0.05),
            )?;
            Ok(json!(r))
        }
        "cons.choose" | "cons.report" | "cons.ctor_error" => {
            let bandit = conservative::ConservativeBandit::new(
                &input["arms"],
                input["safeArm"].as_str().unwrap_or("balanced"),
                input["epsilon"].as_f64().unwrap_or(0.05),
                input["delta"].as_f64().unwrap_or(0.05),
                input["priorMass"].as_f64().unwrap_or(2.0),
            );
            match input.get("op").and_then(Value::as_str).unwrap_or("") {
                "cons.ctor_error" => bandit.map(|_| json!("ok")),
                "cons.report" => Ok(bandit?.report()),
                _ => Ok(bandit?.choose(input["thompson"].as_str())),
            }
        }
        "cons.roundtrip" => {
            let b = conservative::ConservativeBandit::from_json(&input["dict"])?;
            Ok(json!({
                "report": b.report(),
                "dict": b.to_json(),
                "guarantee": b.guarantee(),
            }))
        }
        other => Err(format!("unknown conservative op {other:?}")),
    }
}

#[test]
fn conservative_fixtures_replay() {
    let cases = fixture_lines("conservative");
    assert!(
        cases.len() >= 20,
        "expected the full conservative fixture set"
    );
    for (no, input, output) in cases {
        let label = input
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| input.get("which").and_then(Value::as_str))
            .or_else(|| input.get("op").and_then(Value::as_str))
            .unwrap_or("?")
            .to_string();
        match (
            output.get("error").and_then(Value::as_str),
            run_conservative_op(&input),
        ) {
            (Some(msg), Err(e)) => assert_eq!(e, msg, "{label} (line {no})"),
            (Some(msg), Ok(v)) => panic!("{label} (line {no}): expected Err({msg}), got {v}"),
            (None, Err(e)) => panic!("{label} (line {no}): unexpected Err({e})"),
            (None, Ok(v)) => {
                let mut errs = Vec::new();
                compare(&v, &output, "$", &mut errs);
                assert!(
                    errs.is_empty(),
                    "{label} (line {no}) diverged:\n{}",
                    errs.join("\n")
                );
            }
        }
    }
}

// -------------------------------------------------------------- ensemble ---

fn run_ensemble_op(input: &Value) -> Result<Value, String> {
    match input.get("op").and_then(Value::as_str).unwrap_or("") {
        "ens.consts" => Ok(json!({
            "EPS_FLOOR": ensemble::EPS_FLOOR,
            "DEFAULT_BLOCK": ensemble::DEFAULT_BLOCK,
            "DEFAULT_WINDOW": ensemble::DEFAULT_WINDOW,
            "DEFAULT_MAX_MEMBERS": ensemble::DEFAULT_MAX_MEMBERS,
            "VOTER_KEYS": ensemble::VOTER_KEYS,
        })),
        "ens.combine" => {
            let candidates: Vec<String> = input["candidates"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|v| v.as_str().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            let options = &input["options"];
            Ok(ensemble::combine_votes(
                &candidates,
                &input["votes"],
                &input["model"],
                options
                    .get("sensDefault")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5),
                options
                    .get("fmassDefault")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.1),
            ))
        }
        "ens.math_exp" => Ok(json!(ensemble::math_exp(
            input["z"].as_f64().unwrap_or(0.0)
        ))),
        "ens.nse_run" => {
            let options = &input["options"];
            let mut ens = ensemble::LearnPPNSE::with_options(
                input["nFeatures"].as_i64().unwrap_or(1),
                options.get("block").and_then(Value::as_i64).unwrap_or(25),
                options.get("window").and_then(Value::as_i64).unwrap_or(40),
                options
                    .get("maxMembers")
                    .and_then(Value::as_i64)
                    .unwrap_or(8),
                options
                    .get("minWeight")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.02),
            )?;
            let stream = input["stream"].as_array().cloned().unwrap_or_default();
            let mut observes = Vec::new();
            for row in &stream {
                let feats = f64s_of(&row[0]);
                let label = row.get(1).and_then(Value::as_f64).unwrap_or(0.0);
                observes.push(ens.observe(&feats, label));
            }
            // JS `probe ? ens.predict(probe) : null`: an EMPTY array probe is
            // truthy and is a valid (zero-width) feature vector
            let predict = match input.get("probe") {
                Some(v) if v.is_array() => json!(ens.predict(&f64s_of(v))),
                _ => Value::Null,
            };
            Ok(json!({
                "observes": observes,
                "weights": ens.member_weights(),
                "predict": predict,
                "dict": ens.to_json(),
            }))
        }
        "ens.train_member" => {
            let rows: Vec<ensemble::Row> = input["rows"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|r| {
                            (
                                f64s_of(&r[0]),
                                r.get(1).and_then(Value::as_f64).unwrap_or(0.0).trunc() as i64,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let (w, b) = ensemble::LearnPPNSE::train_member(&rows);
            Ok(json!([w, b]))
        }
        "ens.ctor_error" => {
            let options = &input["options"];
            ensemble::LearnPPNSE::with_options(
                input["nFeatures"].as_i64().unwrap_or(1),
                options.get("block").and_then(Value::as_i64).unwrap_or(25),
                options.get("window").and_then(Value::as_i64).unwrap_or(40),
                options
                    .get("maxMembers")
                    .and_then(Value::as_i64)
                    .unwrap_or(8),
                options
                    .get("minWeight")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.02),
            )
            .map(|_| json!("ok"))
        }
        "ens.nse_roundtrip" => {
            let ens = ensemble::LearnPPNSE::from_json(&input["dict"])?;
            let predict = match input.get("probe") {
                Some(v) if v.is_array() => json!(ens.predict(&f64s_of(v))),
                _ => Value::Null,
            };
            Ok(json!({
                "dict": ens.to_json(),
                "weights": ens.member_weights(),
                "predict": predict,
            }))
        }
        "ens.compare" => {
            let stream = f64s_of(&input["stream"]);
            ensemble::compare_with_adwin(&stream, input["block"].as_i64().unwrap_or(10))
        }
        other => Err(format!("unknown ensemble op {other:?}")),
    }
}

#[test]
fn ensemble_fixtures_replay() {
    let cases = fixture_lines("ensemble");
    assert!(cases.len() >= 35, "expected the full ensemble fixture set");
    for (no, input, output) in cases {
        let label = input
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| input.get("op").and_then(Value::as_str))
            .unwrap_or("?")
            .to_string();
        match (
            output.get("error").and_then(Value::as_str),
            run_ensemble_op(&input),
        ) {
            (Some(msg), Err(e)) => assert_eq!(e, msg, "{label} (line {no})"),
            (Some(msg), Ok(v)) => panic!("{label} (line {no}): expected Err({msg}), got {v}"),
            (None, Err(e)) => panic!("{label} (line {no}): unexpected Err({e})"),
            (None, Ok(v)) => {
                let mut errs = Vec::new();
                compare(&v, &output, "$", &mut errs);
                assert!(
                    errs.is_empty(),
                    "{label} (line {no}) diverged:\n{}",
                    errs.join("\n")
                );
            }
        }
    }
}
