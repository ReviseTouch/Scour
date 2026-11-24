//! Requests whose wording is a decision: each face asking its own way once let
//! the rail and the count disagree with the list.

use scour_core::FacetBy;
use scour_proto::Request;

/// How many matches a search counts while somebody is typing. At 100,000 `ra`
/// visits 1,208,951 rows (23.1 ms); at a thousand, 35,743 (0.9 ms). A face
/// says `1000+` until [`count_all`] answers.
pub const TYPING_CAP: u32 = 1_000;

/// The rail: kinds and ages in one request, always both. The scan cap follows
/// the groups asked, so kinds alone sample the first 200,000 rows the walk
/// reaches — a rail off by a sixth. The walk is the cost, not the groups.
pub fn rail_facets(query: String) -> Request {
    Request::Facets {
        query,
        by: vec![
            FacetBy::Kind,
            FacetBy::Age {
                edges: scour_ui::bar_edges(),
            },
        ],
    }
}

/// The whole count, whatever it costs: 18 ms of a 37 ms keystroke uncapped,
/// so a face asks for it once the typing stops.
pub fn count_all(query: String) -> Request {
    Request::Count {
        query,
        cap: u32::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rail_always_asks_for_both_groups() {
        let Request::Facets { by, .. } = rail_facets("a".into()) else {
            panic!("not facets");
        };
        assert!(matches!(by[..], [FacetBy::Kind, FacetBy::Age { .. }]));
    }
}
