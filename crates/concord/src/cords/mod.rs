//! One module per CORD document. CORD-07 (audio/video) is unimplemented, and
//! CORD-08's timer rides the Chat and Control planes it edits rather than owning a file.

pub mod cord01;
pub mod cord02;
pub mod cord03;
pub mod cord04;
pub mod cord05;
pub mod cord06;
