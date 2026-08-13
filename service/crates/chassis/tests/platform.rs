use chassis::platform::{PlatformClient, MAX_PAGES};
use httptest::{matchers::*, responders::*, Expectation, Server};

#[test]
fn single_page_fetch_parses_items_and_sends_token() {
    let server = Server::run();
    server.expect(
        Expectation::matching(all_of![
            request::method_path("GET", "/items"),
            request::headers(contains(("authorization", "Bearer test-token"))),
        ])
        .times(1)
        .respond_with(json_encoded(serde_json::json!([{"id": 1, "name": "one"}]))),
    );
    let client =
        PlatformClient::with_base_url(&server.url("/").to_string(), Some("test-token".to_string()));
    let items = client.get_paginated("/items").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "one");
}

#[test]
fn pagination_follows_link_headers_and_stops() {
    let server = Server::run();
    let page2_url = server.url("/items2");
    server.expect(
        Expectation::matching(request::method_path("GET", "/items"))
            .times(1)
            .respond_with(
                json_encoded(serde_json::json!([{"id": 1}]))
                    .append_header("Link", format!("<{page2_url}>; rel=\"next\"")),
            ),
    );
    server.expect(
        Expectation::matching(request::method_path("GET", "/items2"))
            .times(1)
            .respond_with(json_encoded(serde_json::json!([{"id": 2}]))),
    );
    let client = PlatformClient::with_base_url(&server.url("/").to_string(), None);
    let items = client.get_paginated("/items").unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], 1);
    assert_eq!(items[1]["id"], 2);
}

#[test]
fn pagination_stops_at_max_pages() {
    let server = Server::run();
    let loop_url = server.url("/loop");
    // Every page links to the next; the client must stop at MAX_PAGES and
    // return the partial results (1 item per page × 100 pages).
    server.expect(
        Expectation::matching(request::method_path("GET", "/loop"))
            .times(MAX_PAGES as usize)
            .respond_with(
                json_encoded(serde_json::json!([{"n": 1}]))
                    .append_header("Link", format!("<{loop_url}>; rel=\"next\"")),
            ),
    );
    let client = PlatformClient::with_base_url(&server.url("/").to_string(), None);
    let items = client.get_paginated("/loop").unwrap();
    assert_eq!(items.len(), MAX_PAGES as usize);
}

#[test]
fn error_status_returns_err() {
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("GET", "/boom"))
            .times(1)
            .respond_with(status_code(500)),
    );
    let client = PlatformClient::with_base_url(&server.url("/").to_string(), None);
    let err = client.get_paginated("/boom").unwrap_err();
    assert!(err.to_string().contains("500"), "err: {err}");
}

#[test]
fn get_readme_decodes_base64_content() {
    let server = Server::run();
    // base64("# Hello\n\n## Install\n") with an embedded newline, as the
    // GitHub API wraps content at 60 chars.
    server.expect(
        Expectation::matching(request::method_path("GET", "/repos/owner/repo/readme"))
            .times(1)
            .respond_with(json_encoded(serde_json::json!({
                "encoding": "base64",
                "content": "IyBIZWxsbwoKIyMg\nSW5zdGFsbAo="
            }))),
    );
    let client = PlatformClient::with_base_url(&server.url("/").to_string(), None);
    let readme = client.get_readme("owner", "repo").unwrap();
    assert_eq!(readme, "# Hello\n\n## Install\n");
}
