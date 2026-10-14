//! Errors raised by the platform side of a delegated computation.
//!
//! A backend, a jailed worker and a transport pipe are all things the
//! host started rather than the program asked for, so a failure in any
//! of them is reported as `backend-failed`. That category is
//! deliberately distinct from `invalid-input`: the first says the
//! request was well formed and the world did not cooperate, the second
//! says the request was not. Collapsing them would let a caller retry a
//! program that can never work.

/// An error from something the host owns, not from the program's input.
pub fn backend(message: impl std::fmt::Display) -> zio_core::error::EvalError {
    zio_core::error::EvalError::custom(format!("backend-failed: {message}"))
}
