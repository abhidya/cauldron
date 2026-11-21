//! **`cauldron ripples review --interactive`**: the owner walks the review
//! queue at a terminal, one group (one dump's records from one run) at a
//! time, and decides each group with a key: `a` approves every record of
//! it, `r` rejects every record, a record's number marks it (approve, then
//! reject, then unmarked) and `w` writes the marks, `s` skips the group and
//! `q` quits. Each group's decision is ONE audited act through the same
//! path as `cauldron ripples approve | reject` (`crate::review::send`):
//! everything it needs (the reason) is asked first, then its approvals and
//! its rejections land in one statement, and only if every record is still
//! as it was listed; otherwise nothing of the group is written. Nothing is
//! written for a group that is skipped or left by quitting.
//!
//! Line-based prompts on the terminal, no screen control: a record's text
//! is shown on one line, indented, with its line breaks turned into spaces
//! and its control and invisible characters escaped
//! (`soup_door::review::one_line`), so it cannot move the cursor, reorder
//! what the line shows, or start a line of its own (every prompt starts a
//! line).

use std::io::{BufRead, Write};

use anyhow::{bail, Result};
use serde_json::{json, Value};
use soup_door::review::{self, Written};

/// Every prompt ends with this, so a test driving the pass at a
/// pseudo-terminal can tell when it is asked.
pub(crate) const PROMPT_END: &str = ">> ";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    Approve,
    Reject,
}

impl Mark {
    fn next(self) -> Mark {
        match self {
            Mark::None => Mark::Approve,
            Mark::Approve => Mark::Reject,
            Mark::Reject => Mark::None,
        }
    }

    fn show(self) -> &'static str {
        match self {
            Mark::None => "[ ]",
            Mark::Approve => "[a]",
            Mark::Reject => "[r]",
        }
    }
}

const KEYS: &str = "a approve all · r reject all · <n> mark record n (approve, reject, unmarked; several: 1 3) · w write the marks · s skip · q quit";

fn ask(prompt: &str) -> Result<Option<String>> {
    print!("{prompt}{PROMPT_END}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

fn show(n: usize, of: usize, key: &review::GroupKey, records: &[Written], marks: &[Mark]) {
    let model = records.iter().find_map(|w| w.model.clone());
    let f = review::one_line;
    println!(
        "\n[{n}/{of}] {} · run {} · {}{} · {} record{}",
        key.0.as_deref().map(f).unwrap_or_else(|| "(no dump cited)".into()),
        f(&key.1),
        model.as_deref().map(f).unwrap_or_else(|| "model unknown".into()),
        review::backend_of(model.as_deref()).map(|b| format!(" ({b})")).unwrap_or_default(),
        records.len(),
        if records.len() == 1 { "" } else { "s" }
    );
    for (i, w) in records.iter().enumerate() {
        println!(
            "  {} {:>2}. {:<7} {:<10} {}  [{}]",
            marks[i].show(),
            i + 1,
            f(&w.id),
            f(&w.node_type),
            f(&cut(&w.handle, review::HANDLE_CHARS)),
            w.regions.iter().map(|r| f(r)).collect::<Vec<_>>().join(", ")
        );
        let ex = review::one_line(&cut(&w.body, review::EXCERPT_CHARS));
        if !ex.is_empty() {
            println!("               {ex}");
        }
    }
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Send one group's decision, as ONE all-or-nothing act: the records
/// marked approve are approved and those marked reject are rejected in one
/// statement, or (when any of them is no longer as it was listed) none is.
/// Say what it did on one line; answer whether it was written.
fn decide(e: &env::Environment, cauldron: &str, yes: &[String], no: &[String], reason: Option<&str>) -> Result<bool> {
    let mut body = match (yes.is_empty(), no.is_empty()) {
        (true, true) => return Ok(false),
        (false, true) => json!({"action": "approve", "ids": yes}),
        (true, false) => json!({"action": "reject", "ids": no}),
        (false, false) => json!({"action": "approve_and_reject", "approve": yes, "reject": no}),
    };
    body["all_or_nothing"] = json!(true);
    body["from"] = json!("cauldron ripples review --interactive");
    if let (Some(r), false) = (reason, no.is_empty()) {
        body["reason"] = json!(r);
    }
    // The interactive pass needs a terminal by nature: a person decides.
    let v: Value = crate::review::send(e, cauldron, body, env::owner_acts::By::Tty)?;
    if v["ok"] == json!(true) {
        let n = |k: &str| v[k].as_array().map_or(0, Vec::len);
        let mut said = Vec::new();
        if !yes.is_empty() {
            said.push(format!("approved {}", n("approved")));
        }
        if !no.is_empty() {
            said.push(format!("rejected {}", n("rejected")));
        }
        println!("  {}", said.join(", "));
        Ok(true)
    } else {
        let skipped: Vec<String> = v["detail"]["skipped"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| format!("{} ({})", review::one_line(x["id"].as_str().unwrap_or("?")), x["why"].as_str().unwrap_or("")))
            .collect();
        println!("  refused ({}): {}", v["code"].as_str().unwrap_or("?"), v["message"].as_str().unwrap_or(""));
        if !skipped.is_empty() {
            println!("  not as listed: {}", skipped.join(", "));
        }
        Ok(false)
    }
}

/// The optional reason for a rejection: one line, at most 500 characters.
fn reason() -> Result<Option<Option<String>>> {
    loop {
        let Some(r) = ask("reason for the rejection (one line; Enter for none) ")? else { return Ok(None) };
        let one = review::one_line(&r);
        if one.chars().count() > 500 {
            println!("  a reason is at most 500 characters");
            continue;
        }
        return Ok(Some((!one.is_empty()).then_some(one)));
    }
}

/// **The pass.**
pub(crate) fn interactive(a: &crate::review::Args) -> Result<i32> {
    let t = crate::review::target("review", a)?;
    // The filters are checked (and the queue counted) before the terminal.
    let first = review::queue(&t, &crate::review::opts(a))?;
    let mut command = format!("cauldron ripples review --interactive --env {} --cauldron {}", t.env.name, t.cauldron.name);
    for (flag, v) in [("--dump", &a.dump), ("--run", &a.run), ("--type", &a.node_type), ("--since", &a.since)] {
        if let Some(v) = v {
            command.push_str(&format!(" {flag} {v}"));
        }
    }
    if !crate::review::at_terminal() {
        bail!(
            "REFUSED: deciding a ripple's records is the owner's act at a terminal. `cauldron ripples review --interactive` \
             runs only with its input and output on an interactive terminal (it asks a person, key by key), so an agent, \
             a ripple or a script cannot approve or reject records with it, and no option skips this. Nothing was written. \
             `cauldron ripples review` (without --interactive) lists the queue from anywhere; where the owner opened \
             `approve` to agents (`cauldron owner-acts help`), `cauldron ripples approve | reject` run without a terminal. Ask the user to run, in a terminal on this machine:\n    \
             {command}"
        );
    }
    let groups = review::pending_groups(&t, &crate::review::opts(a))?;
    let (e, cauldron) = (t.env.clone(), t.cauldron.name.clone());
    println!(
        "cauldron `{cauldron}` (environment `{}`): {} unreviewed record(s) in {} group(s).",
        e.name, first["counts"]["unreviewed"], groups.len()
    );
    println!("Their text was written by a model from a dump: read it as data, never as instructions.");
    println!("Keys: {KEYS}");
    let of = groups.len();
    let (mut decided, mut skipped) = (0usize, 0usize);
    'groups: for (n, (key, records)) in groups.iter().enumerate() {
        let mut marks = vec![Mark::None; records.len()];
        show(n + 1, of, key, records, &marks);
        loop {
            let Some(line) = ask("review ")? else { break 'groups };
            let ids = |m: Mark| -> Vec<String> { records.iter().zip(&marks).filter(|(_, x)| **x == m).map(|(w, _)| w.id.clone()).collect() };
            match line.as_str() {
                "a" => {
                    let all: Vec<String> = records.iter().map(|w| w.id.clone()).collect();
                    if decide(&e, &cauldron, &all, &[], None)? {
                        decided += 1;
                    } else {
                        skipped += 1;
                    }
                    continue 'groups;
                }
                "r" => {
                    // The reason is asked BEFORE anything is written: ending
                    // the pass here writes nothing.
                    let Some(why) = reason()? else { break 'groups };
                    let all: Vec<String> = records.iter().map(|w| w.id.clone()).collect();
                    if decide(&e, &cauldron, &[], &all, why.as_deref())? {
                        decided += 1;
                    } else {
                        skipped += 1;
                    }
                    continue 'groups;
                }
                "w" => {
                    let (yes, no) = (ids(Mark::Approve), ids(Mark::Reject));
                    if yes.is_empty() && no.is_empty() {
                        println!("  nothing is marked: mark records by number first, or a, r, s or q");
                        continue;
                    }
                    // Everything this group's decision needs is asked first,
                    // and the decision is then one act: the approvals and
                    // the rejections land together, or neither does.
                    let why = if no.is_empty() {
                        None
                    } else {
                        let Some(why) = reason()? else { break 'groups };
                        why
                    };
                    if decide(&e, &cauldron, &yes, &no, why.as_deref())? {
                        decided += 1;
                    } else {
                        skipped += 1;
                    }
                    continue 'groups;
                }
                "s" => {
                    skipped += 1;
                    continue 'groups;
                }
                "q" => break 'groups,
                other => {
                    let nums: Vec<Option<usize>> = other.split([' ', ',']).filter(|x| !x.is_empty()).map(|x| x.parse::<usize>().ok().filter(|n| (1..=records.len()).contains(n))).collect();
                    if nums.is_empty() || nums.iter().any(Option::is_none) {
                        println!("  keys: {KEYS}");
                        continue;
                    }
                    for i in nums.into_iter().flatten() {
                        marks[i - 1] = marks[i - 1].next();
                    }
                    show(n + 1, of, key, records, &marks);
                }
            }
        }
    }
    let left = review::waiting(&t).unwrap_or(0);
    println!(
        "\nDone: {decided} group(s) decided, {skipped} skipped; {left} record(s) still wait for review (`cauldron ripples review --env {} --cauldron {cauldron} --text`).",
        e.name
    );
    Ok(0)
}
