//! One request on the wire and the frames that answer it.

use super::*;
use scour_core::Error;
use serde::{Deserialize, Serialize};

/// The protocol version. Bumped when an existing message changes shape;
/// adding a variant does not need it.
pub const VERSION: u32 = 1;

/// One request, with the id its reply will carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// One frame of one answer. Almost every request is answered by exactly one; see
/// `more` for [`Request::Export`], which is answered by a run of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    /// Another frame with this id follows. Without the marker a client reading one
    /// line per answer takes the first piece as the whole and leaves the rest for the
    /// next request. Defaulted and omitted when false, so old frames read unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub more: bool,
    #[serde(flatten)]
    pub outcome: Outcome,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Reply {
    /// A whole answer, in one frame.
    pub fn whole(id: u64, outcome: Outcome) -> Reply {
        Reply {
            id,
            more: false,
            outcome,
        }
    }

    /// A piece, with more to come.
    pub fn piece(id: u64, response: Response) -> Reply {
        Reply {
            id,
            more: true,
            outcome: Outcome::Ok(response),
        }
    }
}

/// **Not boxed.** One of these exists per request and the large variant is the answer
/// itself, so boxing would add an allocation to save one copy.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Response),
    Error(Error),
}
