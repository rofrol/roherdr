//! [`WorkerSupervisor`](super::WorkerSupervisor)'s methods by verb: the
//! supervisor's struct and shared state stay in `workers/mod.rs`.

pub(super) mod commands;
pub(super) mod handoff;
pub(super) mod lifecycle;
pub(super) mod questions;
pub(super) mod takeover;
pub(super) mod views;
