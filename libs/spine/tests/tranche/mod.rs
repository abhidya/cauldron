//! The tranche's tests, hosted by `lock_behaviour` through one
//! `#[path]` line so the crate keeps one test binary per concern
//! Each test file is one `mod <file>;` here.
//! Tests are named after the numbered case they own (`t19_<n>_…`) or the
//! guard they check (`g<n>_…`).

mod record;

mod staging;

mod landing;

mod locks;

mod crash;

mod regions;
