//! Routes asked over HTTP, as the page asks them, of a service double on a
//! socket of the test's own: never the running service.

use super::*;
use scour_core::{
    Caps, Facet, FacetBy, FacetGroup, FacetResponse, SourceId, SourceInfo, SourceKind,
};
use scour_proto::Outcome;
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A service that answers every request with `answer` and keeps what it was asked.
struct Double {
    addr: String,
    asked: Arc<Mutex<Vec<Request>>>,
    _dir: tempfile::TempDir,
}

fn double(answer: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Double {
    // Short, under /tmp: a socket path past 108 bytes cannot be bound.
    let dir = tempfile::Builder::new()
        .prefix("sw")
        .tempdir_in("/tmp")
        .expect("tempdir");
    let addr = dir.path().join("s").to_string_lossy().into_owned();
    let server = scour_ipc::Server::bind(&addr).expect("bind the double");
    let asked = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&asked);
    std::thread::spawn(move || {
        server.serve(
            move |request, _| {
                let reply = answer(&request);
                kept.lock().unwrap().push(request);
                Outcome::Ok(reply)
            },
            Arc::new(AtomicBool::new(false)),
        )
    });
    Double {
        addr,
        asked,
        _dir: dir,
    }
}

impl Double {
    fn asked(&self) -> Vec<Request> {
        self.asked.lock().unwrap().clone()
    }

    /// One exchange through [`serve`]: the status line and the body.
    fn get(&self, target: &str, origin: Option<&str>) -> (String, serde_json::Value) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let mut browser = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let (bridge, _) = listener.accept().expect("accept");
        let pool = Pool::new(self.addr.clone(), 1);
        let addr = self.addr.clone();
        let doing = Doing {
            launch: false,
            run: false,
            preview: false,
            pictures: false,
        };
        let served = std::thread::spawn(move || serve(bridge, &pool, &addr, TOKEN, doing));
        let origin = origin.map_or_else(String::new, |o| format!("Origin: {o}\r\n"));
        write!(
            browser,
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{origin}\r\n"
        )
        .unwrap();
        let mut raw = String::new();
        browser.read_to_string(&mut raw).unwrap();
        served.join().unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status = head.lines().next().unwrap_or_default().to_owned();
        (status, serde_json::from_str(body).unwrap_or_default())
    }
}

fn sources() -> Vec<SourceInfo> {
    vec![SourceInfo {
        id: SourceId(2),
        name: "sistem".into(),
        kind: SourceKind::Local,
        roots: vec!["/usr".into(), "/etc".into()],
        caps: Caps::WATCH,
    }]
}

#[test]
fn sources_come_back_as_the_service_sent_them() {
    let svc = double(|_| Response::Sources { sources: sources() });
    let (status, body) = svc.get(&format!("/api/sources?t={TOKEN}"), None);
    assert_eq!(status, "HTTP/1.1 200 OK");
    let want = serde_json::json!({ "sources": serde_json::to_value(sources()).unwrap() });
    assert_eq!(body, want);
    assert_eq!(body["sources"][0]["name"], "sistem");
    assert_eq!(body["sources"][0]["roots"][1], "/etc");
    assert_eq!(svc.asked(), vec![Request::Sources {}]);
}

#[test]
fn sources_are_refused_without_the_token_or_from_another_origin() {
    let svc = double(|_| Response::Sources { sources: sources() });
    for target in ["/api/sources", "/api/sources?t=", "/api/sources?t=0123"] {
        let (status, _) = svc.get(target, None);
        assert_eq!(status, "HTTP/1.1 403 Forbidden", "{target}");
    }
    let (status, _) = svc.get(
        &format!("/api/sources?t={TOKEN}"),
        Some("http://evil.example"),
    );
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert!(svc.asked().is_empty(), "the service was asked anyway");
}

/// Answers `facets` with one group per question, each holding one row.
fn facets_double() -> Double {
    double(|request| match request {
        Request::Facets { by, .. } => Response::Facets(FacetResponse {
            groups: by
                .iter()
                .map(|b| FacetGroup {
                    by: b.clone(),
                    facets: vec![Facet {
                        key: "older".into(),
                        count: 3,
                    }],
                })
                .collect(),
            total: 3,
            ..Default::default()
        }),
        _ => Response::Accepted,
    })
}

#[test]
fn several_age_lists_come_back_told_apart_by_their_edges() {
    let svc = facets_double();
    let (status, body) = svc.get(
        &format!("/api/facets?t={TOKEN}&q=rapor&by=ext,age,age&top=8&edges=0,1,6,29,364%3B1,2,3"),
        None,
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    let groups = body["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0]["by"], "ext");
    assert_eq!(groups[0]["top"], 8);
    assert_eq!(groups[1]["edges"], serde_json::json!([0, 1, 6, 29, 364]));
    assert_eq!(groups[2]["edges"], serde_json::json!([1, 2, 3]));
    assert_eq!(groups[2]["facets"][0]["key"], "older");
    match &svc.asked()[..] {
        [Request::Facets { query, by }] => {
            assert_eq!(query, "rapor");
            assert_eq!(by[0], FacetBy::Ext { top: 8 });
            assert_eq!(by.len(), 3);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn exact_asks_an_age_group_and_leaves_it_out_of_the_reply() {
    let svc = facets_double();
    let (_, body) = svc.get(&format!("/api/facets?t={TOKEN}&by=kind&exact=1"), None);
    let groups = body["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["by"], "kind");
    match &svc.asked()[..] {
        [Request::Facets { by, .. }] => {
            assert!(matches!(by[..], [FacetBy::Kind, FacetBy::Age { .. }]))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn facets_are_refused_without_the_token() {
    let svc = facets_double();
    let (status, _) = svc.get("/api/facets?by=kind&exact=1", None);
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert!(svc.asked().is_empty());
}

/// The disk tiles want exact per-disk counts: the cap goes through whole.
#[test]
fn a_count_passes_a_cap_up_to_the_largest_there_is() {
    let svc = double(|_| Response::Count {
        total: 7,
        capped: false,
        took_us: 1,
        misread: Default::default(),
    });
    svc.get(
        &format!("/api/count?t={TOKEN}&q=file:&cap=4294967295"),
        None,
    );
    svc.get(&format!("/api/count?t={TOKEN}&q=file:"), None);
    let caps: Vec<u32> = svc
        .asked()
        .iter()
        .map(|r| match r {
            Request::Count { cap, .. } => *cap,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(caps, [u32::MAX, u32::MAX]);
}
