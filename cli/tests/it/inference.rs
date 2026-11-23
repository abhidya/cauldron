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
    run_with(root, args, &[])
}

/// [`run`] with `vars` set on the child.
fn run_with(root: &std::path::Path, args: &[&str], vars: &[(&str, &str)]) -> (i32, String, String) {
    let o = Command::new(CAULDRON)
        .args(args)
        .envs(vars.iter().copied())
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

/// `models pull`: the catalogue, a name not in it, the plan it prints before
/// anything is fetched, the refusal without a yes away from a terminal, and
/// the download itself refused under isolation. Nothing is downloaded and
/// nothing is created under the root's models directory in any of them.
#[test]
fn in2_pull_lists_its_catalogue_and_downloads_nothing_without_consent() {
    let s = scratch("in2");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    let (rc, out, err) = run(&s, &["inference", "models", "pull", "--list"]);
    assert_eq!(rc, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let cat = v["catalogue"].as_array().unwrap();
    assert_eq!(cat.len(), 2, "exactly the two measured models: {v}");
    assert_eq!(cat[0]["name"], "qwen");
    assert_eq!(cat[0]["bytes"], 36_903_139_616u64);
    assert_eq!(cat[0]["licence"], "Apache-2.0");
    assert!(cat[0]["repository"].as_str().unwrap().starts_with("https://huggingface.co/ggml-org/Qwen3.6-35B-A3B-GGUF/tree/"), "{v}");
    assert_eq!(cat[1]["name"], "gpt-oss");
    assert_eq!(cat[1]["bytes"], 63_387_346_208u64);
    assert_eq!(v["redirects_followed_to"][0], "us.aws.cdn.hf.co");

    let (rc, _, err) = run(&s, &["inference", "models", "pull", "llama-7b", "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("not in this release's catalogue") && err.contains("Nothing was downloaded") && err.contains("models add"), "{err}");

    // No --yes, stdin not a terminal: the plan, then a refusal (or, on a
    // disk too small for it, the disk refusal first). Either way nothing.
    let (rc, out, err) = run(&s, &["inference", "models", "pull", "qwen", "--env", "dev"]);
    assert_ne!(rc, 0, "{out}{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["would_download"]["licence"], "Apache-2.0");
    assert_eq!(v["would_download"]["size"], "34.37 GiB");
    let dest = s.join("root/models/qwen3.6-35b-a3b-q8/Qwen3.6-35B-A3B-Q8_0.gguf");
    assert_eq!(v["would_download"]["destination"], dest.display().to_string());
    assert!(err.contains("Nothing was downloaded") && (err.contains("pass --yes") || err.contains("is free on the disk")), "{err}");
    assert!(!s.join("root/models").exists(), "nothing created before consent");

    // --yes: the download itself is refused under isolation (or the disk
    // refusal comes first); nothing is created.
    let (rc, _, err) = run(&s, &["inference", "models", "pull", "gpt-oss", "--yes", "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("Nothing was downloaded") && (err.contains("isolated process") || err.contains("is free on the disk")), "{err}");
    assert!(!s.join("root/models").exists(), "nothing created");
    let (_, out, _) = run(&s, &["inference", "models", "list", "--env", "dev"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["models"].as_array().unwrap().len(), 0, "nothing registered");
}

/// **A download's consent is not an owner act.** With every owner act
/// opened to agents, both by the owner's recorded choice and by
/// `CAULDRON_OWNER_ACTS=open` in the command's own environment, `models
/// pull` away from a terminal and without `--yes` still prints its plan and
/// refuses, creating and registering nothing: `owner_acts` neither opens
/// nor closes it. (With the setting back at `terminal`, `--yes` is still
/// what the command asks for, and the refusal that follows is isolation's
/// or the disk's, never an owner-act refusal.)
#[test]
fn in4_pull_asks_for_its_own_yes_whatever_owner_acts_says() {
    let s = scratch("in4");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    let open = [("CAULDRON_OWNER_ACTS", "open")];
    let (rc, out, err) = run_with(&s, &["owner-acts", "open"], &open);
    assert_eq!(rc, 0, "{out}{err}");
    let (rc, out, err) = run_with(&s, &["inference", "models", "pull", "qwen", "--env", "dev"], &open);
    assert_ne!(rc, 0, "{out}{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["would_download"]["size"], "34.37 GiB", "{v}");
    assert!(err.contains("Nothing was downloaded") && (err.contains("pass --yes") || err.contains("is free on the disk")), "{err}");
    assert!(!err.contains("owner-acts") && !err.contains("CAULDRON_OWNER_ACTS"), "no owner-act wording: {err}");
    assert!(!s.join("root/models").exists(), "nothing created without a yes");
    assert_eq!(run(&s, &["owner-acts", "terminal"]).0, 0);
    let (rc, _, err) = run(&s, &["inference", "models", "pull", "qwen", "--yes", "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("Nothing was downloaded") && (err.contains("isolated process") || err.contains("is free on the disk")), "{err}");
    assert!(!err.contains("owner-acts") && !err.contains("interactive terminal"), "not refused as an owner act: {err}");
    assert!(!s.join("root/models").exists(), "nothing created");
}

/// `status` says plainly what is missing before a local model can answer:
/// the service, `llama-server` (with how to get it and the minimum build),
/// a model (with `models pull` and `models add`), and a model over the
/// memory budget. Run with the service stopped, on a PATH without
/// llama-server, then with a stand-in that states too old a build, then one
/// new enough.
#[test]
fn in3_status_says_what_is_missing_and_how_to_get_it() {
    let s = scratch("in3");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    let status = |path: &str| -> (Value, String) {
        let o = Command::new(CAULDRON)
            .args(["inference", "status", "--env", "dev"])
            .env("CAULDRON_HOME", s.join("root"))
            .env("HOME", s.join("home"))
            .env("PATH", path)
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0));
        (serde_json::from_str(&String::from_utf8_lossy(&o.stdout)).unwrap(), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let (v, err) = status("/usr/bin:/bin");
    let m: Vec<String> = v["missing"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    assert_eq!(m.len() >= 3, true, "{m:?}");
    assert!(m[0].contains("not installed") && m[0].contains("build inference") && m[0].contains("start inference"), "{m:?}");
    assert!(m[1].contains("No usable llama-server") && m[1].contains("brew install llama.cpp") && m[1].contains("build 6000 or later"), "{m:?}");
    assert!(m[2].contains("No model is registered") && m[2].contains("models pull qwen") && m[2].contains("models add"), "{m:?}");
    assert!(err.contains("missing: No usable llama-server"), "{err}");

    let bin = s.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("llama-server");
    std::fs::write(&fake, "#!/bin/sh\necho 'version: 0.9.0 (build 5000, abc)'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let (v, _) = status(&path);
    let all = v["missing"].to_string();
    assert!(all.contains("build 5000; build 6000 or later is needed") && all.contains("brew upgrade llama.cpp"), "{all}");

    // New enough, and a model registered at a context over the budget.
    std::fs::write(&fake, "#!/bin/sh\necho 'version: 0.9.0 (build 12110, abc)'\n").unwrap();
    let model = s.join("models").join("big.gguf");
    inference_registry::gguf::write_header(
        &model,
        &[
            ("general.architecture", Meta::Str("llama".into())),
            ("llama.block_count", Meta::Int(400)),
            ("llama.attention.head_count", Meta::Int(64)),
            ("llama.attention.head_count_kv", Meta::Int(64)),
            ("llama.embedding_length", Meta::Int(8192)),
        ],
        2048,
    )
    .unwrap();
    let (rc, _, err) = run(&s, &["inference", "models", "add", "big", &model.display().to_string(), "--context", "1048576", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    let (v, _) = status(&path);
    let all = v["missing"].to_string();
    assert!(v["runtime_on_this_path"]["build"] == 12110, "{v}");
    assert!(!all.contains("llama-server") && !all.contains("No model is registered"), "{all}");
    assert!(all.contains("`big` would be refused") && all.contains("memory_budget_gib"), "{all}");
}
