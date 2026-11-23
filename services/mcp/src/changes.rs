//! **`soup_changes`**, as `soup_door::changes` answers it (the command line's
//! `cauldron soup changes` answers the same), with this door's wait: a call
//! with `wait_secs` sleeps on the service's [`Pulse`], which every write the
//! service lands bumps, so it answers as soon as one of them lands; a write
//! from another process (a `cauldron soup import` while this door has not
//! opened the cauldron) is seen within [`LOOK`]. The grant is re-read on
//! every wake. The wait holds no writer, no lock and no open store, and it
//! ends at once when its caller hangs up (`crate::door::Hangup`).

pub use soup_door::changes::{after_budget, parse_since, Pulse, Wake, IDS_PER_CHANGE, MAX_LIMIT, NEXT, WAIT_MAX_SECS};

use serde_json::Value;
use shapewire::envelope::Refusal;
use soup_door::Target;

use crate::refusal;

/// The longest a waiting call sleeps between two looks at the store.
pub const LOOK: std::time::Duration = std::time::Duration::from_secs(1);

/// `soup_changes` without a wait.
pub fn changes(t: &Target, since: Option<&str>, limit: Option<usize>) -> Result<Value, Refusal> {
    soup_door::changes::changes(t, since, limit).map_err(refusal)
}

/// `soup_changes` with `wait_secs` (and `since`): see the module docs.
pub fn waiting(
    t: &Target,
    since: &str,
    limit: Option<usize>,
    wait_secs: u64,
    pulse: &Pulse,
    allowed: &mut dyn FnMut() -> Result<(), Refusal>,
) -> Result<Value, Refusal> {
    // The door's own refusal is kept whole and handed back as it was.
    let mut stopped: Option<Refusal> = None;
    let mut gate = || match allowed() {
        Ok(()) => Ok(()),
        Err(r) => {
            let e = soup_door::Error::new(soup_door::Code::NotGranted, r.message.clone());
            stopped = Some(r);
            Err(e)
        }
    };
    let out = soup_door::changes::changes_waiting(t, since, limit, std::time::Duration::from_secs(wait_secs), pulse, LOOK, &mut gate);
    match (out, stopped) {
        (Err(_), Some(r)) => Err(r),
        (out, _) => out.map_err(refusal),
    }
}
