//! **The tool catalogue**, kept in `soup_door::catalogue` so the stdio
//! bridge answers `tools/list` with the same list when this service is not
//! running. Re-exported here for the door.

pub use soup_door::catalogue::*;
