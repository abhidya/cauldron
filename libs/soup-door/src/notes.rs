//! **An edge's `note`, bounded at the door.**
//!
//! The grammars (`workspace@6`, `cauldron@1`) let every edge carry an
//! optional free-text `note` and put no bound on it: the bounded edge text
//! they name (`admits` on `in_region`, `rejects` on `rejected_by`) is 1,000
//! characters at most, and a note had none. So the door bounds every NEW
//! note at that same [`EDGE_NOTE_MAX`]: a record's edge (`soup_put`,
//! `cauldron soup import`, a ripple's batch), a link (`soup_link`) and a
//! claim (`soup_claim`, `cauldron soup claim`). The published schema files
//! are not changed, and nothing already stored is refused or rewritten: a
//! longer note written before the bound reads back cut to it, named in the
//! answer's `truncated` list as every cut field is.

use serde_json::Value;

use crate::cut::Cut;
use crate::{Code, Error, Result};

/// The most characters a new edge note may hold, and the most a read shows
/// of a stored one.
pub const EDGE_NOTE_MAX: usize = 1_000;

/// `note`, on the edge `what` names, if it is over the bound: the refusal.
pub fn check(note: &str, what: &str) -> Result<()> {
    let n = note.chars().count();
    if n > EDGE_NOTE_MAX {
        return Err(Error::new(
            Code::Usage,
            format!(
                "the note on {what} is {n} characters; an edge's note is at most {EDGE_NOTE_MAX} (the bound the grammar \
                 puts on an edge's text). Nothing was written. Shorten it, or put the text in a record's body"
            ),
        ));
    }
    Ok(())
}

/// Every record's edge notes, checked: the first over the bound refuses the
/// whole batch, before anything is judged or written (as a review flag does).
pub fn refuse_long_notes(records: &[Value]) -> Result<()> {
    for (i, r) in records.iter().enumerate() {
        let label = r.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("#{i}"));
        for (j, e) in r.get("edges").and_then(Value::as_array).into_iter().flatten().enumerate() {
            if let Some(n) = e.get("note").and_then(Value::as_str) {
                let kind = e.get("kind").and_then(Value::as_str).unwrap_or("?");
                let to = e.get("to").and_then(Value::as_str).unwrap_or("?");
                check(n, &format!("record `{label}`'s edge {j} (`{kind}` to `{to}`)"))?;
            }
        }
    }
    Ok(())
}

/// A stored note as a read shows it: at most [`EDGE_NOTE_MAX`] characters,
/// the cut noted in `cut` (`field` names the edge, e.g. `edges/2/note`). A
/// read's per-field `--max-chars` does not apply to notes, as before; the
/// whole-reply bound (`max_chars` at the door) still does.
pub fn shown(cut: &mut Cut, id: &str, field: &str, note: &str) -> String {
    cut.text(id, field, note, EDGE_NOTE_MAX)
}
