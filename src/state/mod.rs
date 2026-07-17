//! Persisted UI state, so the tool reopens the way you left it: the last region
//! selection and the chosen pre-capture delay. Stored as RON in
//! `$XDG_STATE_HOME/cosmic-capture-kit/state.ron` (falling back to the cache dir).

mod schema;
mod store;

pub use self::schema::*;
pub use self::store::*;
