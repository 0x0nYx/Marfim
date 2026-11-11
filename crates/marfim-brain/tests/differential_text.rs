//! Differential replay tests for the P4-C text-intelligence cluster:
//! fts, phonetics, confusables, dp, dedupe. Every line of
//! `fixtures/intel/<module>.jsonl` is an (input, output) pair RECORDED BY
//! RUNNING the Node oracle (`preserved/oracle/src/intel/*.js`) via the
//! generator scripts in `scripts/fixture-text-*.js`; this file replays the
//! inputs against the Rust port and compares recursively:
//!
//! - ints byte-exact (when both JSON numbers are integral)
//! - f64 within 1 ulp (`marfim_brain::util::ulp_close`) — the oracle's
//!   transcendental call sites (Math.log/log10/pow) may legally differ by
//!   the platform libm's last bit; everything else is bit-exact in practice
//! - strings byte-exact, arrays order-sensitive, objects key-wise

use serde_json::{json, Map, Value};

use marfim_brain::intel::{confusables, dedupe, dp, fts, phonetics};
use marfim_brain::util::ulp_close;

fn fixtures_path(name: &str) -> String {
    format!(
        "{}/../../fixtures/intel/{name}.jsonl",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn load_fixture(name: &str) -> Vec<Value> {
    let path = fixtures_path(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {name}: {e} (path {path})"));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).unwrap_or_else(|e| panic!("{name} line {i}: {e}")))
        .collect()
}

fn cmp_num(e: &serde_json::Number, a: &serde_json::Number, path: &str) -> Result<(), String> {
    let ei = e.as_i64();
    let ai = a.as_i64();
    if let (Some(x), Some(y)) = (ei, ai) {
        if x == y {
            return Ok(());
        }
        return Err(format!("{path}: int {x} != {y}"));
    }
    let (fx, fy) = (
        e.as_f64().unwrap_or(f64::NAN),
        a.as_f64().unwrap_or(f64::NAN),
    );
    if ulp_close(fx, fy) {
        Ok(())
    } else {
        Err(format!("{path}: float {fx} != {fy}"))
    }
}

fn diff_json(e: &Value, a: &Value, path: &str) -> Result<(), String> {
    match (e, a) {
        (Value::Object(eo), Value::Object(ao)) => {
            for k in eo.keys() {
                if !ao.contains_key(k) {
                    return Err(format!("{path}.{k}: missing from actual"));
                }
            }
            for k in ao.keys() {
                if !eo.contains_key(k) {
                    return Err(format!("{path}.{k}: unexpected in actual"));
                }
            }
            for k in eo.keys() {
                diff_json(&eo[k], &ao[k], &format!("{path}.{k}"))?;
            }
            Ok(())
        }
        (Value::Array(ev), Value::Array(av)) => {
            if ev.len() != av.len() {
                return Err(format!("{path}: array len {} != {}", ev.len(), av.len()));
            }
            for (i, (x, y)) in ev.iter().zip(av.iter()).enumerate() {
                diff_json(x, y, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        (Value::String(es), Value::String(as_)) => {
            if es == as_ {
                Ok(())
            } else {
                Err(format!("{path}: string {es:?} != {as_:?}"))
            }
        }
        (Value::Bool(eb), Value::Bool(ab)) => {
            if eb == ab {
                Ok(())
            } else {
                Err(format!("{path}: bool {eb} != {ab}"))
            }
        }
        (Value::Null, Value::Null) => Ok(()),
        (Value::Number(en), Value::Number(an)) => cmp_num(en, an, path),
        _ => Err(format!("{path}: type mismatch {e} vs {a}")),
    }
}

fn replay_all(name: &str, cases: &[Value], replay: impl Fn(&Value) -> Value) {
    for (i, case) in cases.iter().enumerate() {
        let expected = &case["output"];
        let actual = replay(&case["input"]);
        if let Err(problem) = diff_json(expected, &actual, "$") {
            let op = case["input"]["op"].as_str().unwrap_or("?");
            let tag = case["input"]["tag"].as_str().unwrap_or("");
            panic!(
                "{name} line {i} (op={op}, tag={tag}): {problem}\n  expected: {expected}\n  actual:   {actual}"
            );
        }
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn opt_f64(v: &Value) -> Option<f64> {
    v.as_f64()
}

// --------------------------------------------------------------------- fts

fn fts_replay(input: &Value) -> Value {
    match input["op"].as_str().unwrap_or_default() {
        "ftsAvailable" => json!(fts::fts_available()),
        "search" => {
            let mut ix = fts::FtsIndex::new();
            for op in input["ops"].as_array().unwrap() {
                match op["op"].as_str().unwrap() {
                    "add" => {
                        ix.add(&s(&op["id"]), &s(&op["text"]));
                    }
                    "addDoc" => {
                        ix.add_doc(&s(&op["id"]), &s(&op["title"]), &s(&op["body"]));
                    }
                    "index" => {
                        let docs: Vec<(String, String, String)> = op["docs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|t| (s(&t[0]), s(&t[1]), s(&t[2])))
                            .collect();
                        ix.index(&docs);
                    }
                    "close" => ix.close(),
                    other => panic!("unknown fts op {other}"),
                }
            }
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .map(|v| v as usize)
                .unwrap_or(10);
            let hits = ix.search(&s(&input["query"]), limit);
            serde_json::to_value(&hits).unwrap()
        }
        other => panic!("unknown fts fixture op {other}"),
    }
}

#[test]
fn fts_differential_replay() {
    let cases = load_fixture("fts");
    assert!(cases.len() >= 20, "expected a real fixture corpus");
    replay_all("fts", &cases, fts_replay);
}

// --------------------------------------------------------------- phonetics

fn phonetics_replay(input: &Value) -> Value {
    match input["op"].as_str().unwrap_or_default() {
        "soundex" => json!(phonetics::soundex(&s(&input["word"]))),
        "charNgrams" => {
            let n = input
                .get("n")
                .and_then(Value::as_u64)
                .map(|v| v as usize)
                .unwrap_or(3);
            let grams = phonetics::char_ngrams(&s(&input["text"]), n);
            let list: Vec<String> = grams.into_iter().collect();
            json!(list)
        }
        "phoneticHitRate" => {
            let empty: Vec<String> = Vec::new();
            let q: Vec<String> = input["queryTokens"]
                .as_array()
                .map(|a| a.iter().map(s).collect())
                .unwrap_or_else(|| empty.clone());
            let a: Vec<String> = input["atomTokens"]
                .as_array()
                .map(|a| a.iter().map(s).collect())
                .unwrap_or_default();
            json!(phonetics::phonetic_hit_rate(&q, &a))
        }
        other => panic!("unknown phonetics fixture op {other}"),
    }
}

#[test]
fn phonetics_differential_replay() {
    let cases = load_fixture("phonetics");
    assert!(cases.len() >= 40, "expected a real fixture corpus");
    replay_all("phonetics", &cases, phonetics_replay);
}

// -------------------------------------------------------------- confusables

fn doc_index_from(input: &Value) -> confusables::DocIndex {
    let pairs: Vec<(String, Vec<String>)> = input["docTokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kv| {
            (
                s(&kv[0]),
                kv[1]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(s)
                    .collect::<Vec<String>>(),
            )
        })
        .collect();
    confusables::DocIndex::new(pairs)
}

fn candidates_from(input: &Value) -> Vec<Option<String>> {
    input["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| if c.is_null() { None } else { Some(s(c)) })
        .collect()
}

fn pairs_from(v: &Value) -> Vec<confusables::ConfusablePair> {
    serde_json::from_value(v.clone()).unwrap()
}

fn confusables_replay(input: &Value) -> Value {
    match input["op"].as_str().unwrap_or_default() {
        "consts" => json!({
            "doc_sim_floor": confusables::DOC_SIM_FLOOR,
            "hand_pinned": confusables::HAND_PINNED.len(),
            "confusable_pairs_len": confusables::load_pairs().len(),
        }),
        "loadPairs" => serde_json::to_value(confusables::load_pairs()).unwrap(),
        "renderLines" => json!(confusables::render_lines()),
        "docSimilarity" => {
            let index = doc_index_from(input);
            json!(confusables::doc_similarity(
                &index,
                &s(&input["a"]),
                &s(&input["b"])
            ))
        }
        "questionFor" => {
            let a: confusables::ToolSpec = serde_json::from_value(input["a"].clone()).unwrap();
            let b: confusables::ToolSpec = serde_json::from_value(input["b"].clone()).unwrap();
            match confusables::question_for(&a, &b) {
                Some(q) => serde_json::to_value(&q).unwrap(),
                None => Value::Null,
            }
        }
        "minePairs" => {
            let index = doc_index_from(input);
            let specs: Vec<confusables::ToolSpec> =
                serde_json::from_value(input["specs"].clone()).unwrap();
            serde_json::to_value(confusables::mine_pairs(&index, &specs)).unwrap()
        }
        "clarify" => {
            let candidates = candidates_from(input);
            let runner = input["runner"]["pairs"]
                .as_array()
                .map(|_| pairs_from(&input["runner"]["pairs"]))
                .or_else(|| {
                    input["runner"]["loadPairs"]
                        .as_bool()
                        .map(|_| pairs_from(&input["runner"]["pairs"]))
                });
            json!(confusables::clarify(
                &s(&input["question"]),
                &candidates,
                runner.as_deref()
            ))
        }
        "clarifyDetail" => {
            let candidates = candidates_from(input);
            let runner = input["runner"]["pairs"]
                .as_array()
                .map(|_| pairs_from(&input["runner"]["pairs"]))
                .or_else(|| {
                    input["runner"]["loadPairs"]
                        .as_bool()
                        .map(|_| pairs_from(&input["runner"]["pairs"]))
                });
            let (q, upgraded, pair) =
                confusables::clarify_detail(&s(&input["question"]), &candidates, runner.as_deref());
            json!({
                "question": q,
                "upgraded": upgraded,
                "pair": pair,
            })
        }
        other => panic!("unknown confusables fixture op {other}"),
    }
}

#[test]
fn confusables_differential_replay() {
    let cases = load_fixture("confusables");
    assert!(cases.len() >= 30, "expected a real fixture corpus");
    replay_all("confusables", &cases, confusables_replay);
}

// ---------------------------------------------------------------------- dp

fn dp_replay(input: &Value) -> Value {
    match input["op"].as_str().unwrap_or_default() {
        "consts" => json!({
            "DEFAULT_EPSILON": dp::DEFAULT_EPSILON,
            "N_SENSITIVITY": dp::N_SENSITIVITY,
            "P_SENSITIVITY": dp::P_SENSITIVITY,
            "TAIL_FLOOR": dp::TAIL_FLOOR,
            "GUARANTEE": dp::GUARANTEE,
            "DIFF_HEADER": dp::DIFF_HEADER,
        }),
        "fixedRng" => {
            let u = opt_f64(&input["u"]).unwrap_or(0.0);
            let mut rng = dp::FixedRng { u };
            json!({ "first": dp::Random::random(&mut rng), "second": dp::Random::random(&mut rng) })
        }
        "laplaceNoise" => {
            let mut rng = dp::FixedRng {
                u: opt_f64(&input["u"]).unwrap_or(0.0),
            };
            match dp::laplace_noise(
                opt_f64(&input["value"]).unwrap_or(0.0),
                opt_f64(&input["sensitivity"]).unwrap_or(1.0),
                opt_f64(&input["epsilon"]).unwrap_or(1.0),
                &mut rng,
            ) {
                Ok(v) => json!(v),
                Err(e) => json!({ "error": e.0 }),
            }
        }
        "renderRows" => {
            let rows: Vec<dp::DiffRow> = serde_json::from_value(input["rows"].clone()).unwrap();
            let date = input
                .get("date")
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!(dp::render_rows(&rows, date))
        }
        "diffId" => {
            let rows: Vec<dp::DiffRow> = serde_json::from_value(input["rows"].clone()).unwrap();
            json!(dp::diff_id(&rows))
        }
        "noiseDiff" => {
            let diff = if input["diff"].is_string() {
                dp::DiffInput::Text(s(&input["diff"]))
            } else {
                dp::DiffInput::Rows(serde_json::from_value(input["diff"].clone()).unwrap())
            };
            let opts_in = input["options"].as_object().cloned().unwrap_or(Map::new());
            let options = dp::NoiseOptions {
                epsilon: opts_in.get("epsilon").and_then(opt_f64),
                seed: opts_in.get("seed").and_then(opt_f64),
                min_count_floor: opts_in
                    .get("minCountFloor")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                date: opts_in
                    .get("date")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                presence_keep: opts_in.get("presenceKeep").and_then(opt_f64),
            };
            match dp::noise_diff(diff, &options) {
                Ok(report) => serde_json::to_value(&report).unwrap(),
                Err(e) => json!({ "error": e.0 }),
            }
        }
        other => panic!("unknown dp fixture op {other}"),
    }
}

#[test]
fn dp_differential_replay() {
    let cases = load_fixture("dp");
    assert!(cases.len() >= 40, "expected a real fixture corpus");
    replay_all("dp", &cases, dp_replay);
}

// ------------------------------------------------------------------ dedupe

fn dedupe_vfs(input: &Value) -> dedupe::Vfs {
    let mut vfs = dedupe::Vfs::new();
    // quarantine/restore fixtures record the pre-state as kind map "fs"
    if let Some(fs) = input["fs"].as_object() {
        for (rel, kind) in fs {
            let node = match kind.as_str().unwrap_or_default() {
                "dir" => dedupe::FsNode::Dir,
                "symlink" => dedupe::FsNode::Symlink,
                _ => dedupe::FsNode::File(Vec::new()),
            };
            vfs.insert(rel.clone(), node);
        }
    }
    // scan fixtures record contents under "files" (+ dirs/symlinks lists)
    if let Some(files) = input["files"].as_object() {
        for (rel, content) in files {
            let bytes = if let Some(b64) = content.get("b64").and_then(Value::as_str) {
                base64_decode_fallback::decode(b64)
            } else {
                content.as_str().unwrap_or_default().as_bytes().to_vec()
            };
            vfs.insert(rel.clone(), dedupe::FsNode::File(bytes));
        }
    }
    for rel in input["dirs"].as_array().unwrap_or(&vec![]) {
        vfs.insert(s(rel), dedupe::FsNode::Dir);
    }
    if let Some(links) = input["symlinks"].as_object() {
        for rel in links.keys() {
            vfs.insert(rel.clone(), dedupe::FsNode::Symlink);
        }
    }
    // the oracle's tree was built with mkdirSync(recursive): every ancestor
    // directory of a recorded node exists on disk even when the fixture only
    // records leaves — materialize those ancestors as Dir nodes.
    let keys: Vec<String> = vfs.keys().cloned().collect();
    for key in keys {
        let mut prefix = String::new();
        for segment in key.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            vfs.entry(prefix.clone()).or_insert(dedupe::FsNode::Dir);
        }
    }
    vfs
}

/// Minimal standard-alphabet base64 decoder for fixture contents (no
/// external deps in the test harness).
mod base64_decode_fallback {
    pub fn decode(input: &str) -> Vec<u8> {
        let mut table = [255u8; 256];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for (i, &c) in alphabet.iter().enumerate() {
            table[c as usize] = i as u8;
        }
        let mut out = Vec::new();
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        for &c in input.as_bytes() {
            let v = table[c as usize];
            if v == 255 {
                continue; // '=' or whitespace
            }
            acc = (acc << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(((acc >> bits) & 0xff) as u8);
            }
        }
        out
    }
}

fn manifests_from(input: &Value) -> std::collections::BTreeMap<String, Vec<dedupe::ManifestEntry>> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(ms) = input["manifests"].as_object() {
        for (qdir, entries) in ms {
            let parsed: Vec<dedupe::ManifestEntry> =
                serde_json::from_value(entries.clone()).unwrap();
            out.insert(qdir.clone(), parsed);
        }
    }
    out
}

fn dedupe_replay(input: &Value) -> Value {
    match input["op"].as_str().unwrap_or_default() {
        "scanDuplicates" => {
            let vfs = dedupe_vfs(input);
            let roots: Vec<String> = input["roots"].as_array().unwrap().iter().map(s).collect();
            let root_refs: Vec<&str> = roots.iter().map(String::as_str).collect();
            let min_size = input.get("minSize").and_then(Value::as_u64).unwrap_or(1);
            let groups = dedupe::scan_duplicates(&vfs, &root_refs, min_size);
            serde_json::to_value(&groups).unwrap()
        }
        "survivalScores" => {
            let now = opt_f64(&input["now"]).unwrap_or(0.0);
            let entries: Vec<(String, Option<f64>)> = input["atimes"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(p, v)| (p.clone(), v.as_f64()))
                .collect();
            let scores = dedupe::survival_scores(&entries, now);
            serde_json::to_value(&scores).unwrap()
        }
        "quarantine" => {
            let vfs = dedupe_vfs(input);
            let manifests = manifests_from(input);
            let mut store = dedupe::QuarantineStore::new(vfs, manifests);
            let paths: Vec<String> = input["paths"].as_array().unwrap().iter().map(s).collect();
            let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            let out = store.quarantine(
                &path_refs,
                &s(&input["qdir"]),
                opt_f64(&input["now"]).unwrap_or(0.0),
                opt_f64(&input["ttl_days"]).unwrap_or(30.0),
            );
            serde_json::to_value(&out).unwrap()
        }
        "restore" => {
            let vfs = dedupe_vfs(input);
            let manifests = manifests_from(input);
            let mut store = dedupe::QuarantineStore::new(vfs, manifests);
            let out = store.restore(&s(&input["qdir"]), opt_f64(&input["now"]).unwrap_or(0.0));
            serde_json::to_value(&out).unwrap()
        }
        other => panic!("unknown dedupe fixture op {other}"),
    }
}

#[test]
fn dedupe_differential_replay() {
    let cases = load_fixture("dedupe");
    assert!(cases.len() >= 25, "expected a real fixture corpus");
    replay_all("dedupe", &cases, dedupe_replay);
}
