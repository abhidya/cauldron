//! `cauldron inference` without its service: the help, registering a model
//! file (recorded, never copied), listing it with its memory estimate,
//! refusing a second of the same name and a file that is not GGUF, removing
//! it (the file stays), and `status` with the service stopped.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use inference_registry::gguf::Meta;
use serde_json::Value;

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");

fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "cauldron-inference-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(p.join("home")).unwrap();
    std::fs::create_dir_all(p.join("models")).unwrap();
    p.canonicalize().unwrap()
}

fn run(root: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(CAULDRON)
        .args(args)
        .env("CAULDRON_HOME", root.join("root"))
        .env("HOME", root.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn in1_models_by_hand_and_status_without_the_service() {
    let s = scratch("in1");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    let (rc, out, _) = run(&s, &["inference", "help"]);
    assert_eq!(rc, 0);
    assert!(out.contains("cauldron inference models add <name> <path>") && out.contains("Nothing is copied"), "{out}");

    let model = s.join("models").join("tiny.gguf");
    inference_registry::gguf::write_header(
        &model,
        &[
            ("general.architecture", Meta::Str("llama".into())),
            ("llama.block_count", Meta::Int(2)),
            ("llama.attention.head_count", Meta::Int(4)),
            ("llama.attention.head_count_kv", Meta::Int(1)),
            ("llama.embedding_length", Meta::Int(256)),
        ],
        2048,
    )
    .unwrap();
    let m = model.display().to_string();
    let (rc, out, err) = run(&s, &["inference", "models", "add", "tiny", &m, "--context", "8192", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["registered"]["name"], "tiny");
    assert_eq!(v["registered"]["path"], m);
    assert_eq!(v["registered"]["sha256"], inference_registry::sha256_file(&model).unwrap());
    assert_eq!(v["registered"]["context"], 8192);
    assert_eq!(v["memory_estimate"]["fits"], true, "{v}");
    assert_eq!(std::fs::read_dir(s.join("models")).unwrap().count(), 1, "nothing copied beside it");
    let reg = s.join("root/data/dev/inference/models.json");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&reg).unwrap().permissions().mode() & 0o777, 0o600);

    let (rc, _, err) = run(&s, &["inference", "models", "add", "tiny", &m, "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("already registered"), "{err}");
    std::fs::write(s.join("models").join("not.gguf"), b"PK\x03\x04 a zip").unwrap();
    let (rc, _, err) = run(&s, &["inference", "models", "add", "zip", &s.join("models").join("not.gguf").display().to_string(), "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("not a GGUF model file") && err.contains("Nothing was registered"), "{err}");

    let (rc, out, _) = run(&s, &["inference", "models", "list", "--env", "dev"]);
    assert_eq!(rc, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["models"][0]["name"], "tiny");
    assert_eq!(v["models"][0]["unchanged"], true);
    assert_eq!(v["memory_budget"], "64.00 GiB");
    assert!(v["models"][0]["memory_estimate"]["how"].as_str().unwrap().contains("× 8192 tokens"), "{v}");

    // `verify` reads the file again; a same-size swap with its times copied
    // is seen by `list` (nothing read) and refused by `verify`.
    let (rc, out, err) = run(&s, &["inference", "models", "verify", "tiny", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("\"verified\": true"), "{out}");
    let other = s.join("other.gguf");
    let mut bytes = std::fs::read(&model).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&other, &bytes).unwrap();
    assert!(Command::new("/usr/bin/touch").arg("-r").arg(&model).arg(&other).status().unwrap().success());
    let saved = s.join("saved.gguf");
    std::fs::rename(&model, &saved).unwrap();
    std::fs::rename(&other, &model).unwrap();
    let (_, out, _) = run(&s, &["inference", "models", "list", "--env", "dev"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["models"][0]["unchanged"], false, "{v}");
    let (rc, out, err) = run(&s, &["inference", "models", "verify", "tiny", "--env", "dev"]);
    assert_eq!(rc, 1);
    assert!(out.contains("\"verified\": false") && err.contains("cauldron inference models add tiny"), "{out} {err}");
    std::fs::rename(&saved, &model).unwrap();
    let (rc, _, err) = run(&s, &["inference", "models", "verify", "tiny", "--env", "dev"]);
    assert_eq!(rc, 0, "the original, back: its sha256 is the one registered: {err}");

    let (rc, out, err) = run(&s, &["inference", "status", "--env", "dev"]);
    assert_eq!(rc, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["service"]["running"], false);
    assert_eq!(v["registered"][0], "tiny");
    assert!(err.contains("start inference"), "{err}");

    let (rc, out, _) = run(&s, &["inference", "models", "remove", "tiny", "--env", "dev"]);
    assert_eq!(rc, 0);
    assert!(out.contains("file_left_at"));
    assert!(model.exists(), "the file stays");
    let (rc, _, err) = run(&s, &["inference", "models", "remove", "tiny", "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("no model named"), "{err}");
    let _ = std::fs::remove_dir_all(&s);
}
