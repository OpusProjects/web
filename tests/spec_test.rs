// What Spec answers about routes, asked of a small OpenAPI document written as
// JSON: every shape the lookups meet in a real service's document, in a few
// paths.
use serde_json::json;
use web_rs::Spec;

fn spec() -> Spec {
    let doc = json!({
        "openapi": "3.1.0",
        "info": { "title": "test", "version": "0" },
        "paths": {
            "/api/v1/things": {
                "get": {
                    "description": "Every thing.",
                    "parameters": [
                        { "name": "limit", "in": "query", "required": false,
                          "description": "At most this many", "schema": { "type": "integer" } },
                        { "$ref": "#/components/parameters/Shared" }
                    ],
                    "responses": {
                        "200": { "description": "The things" },
                        "401": { "$ref": "#/components/responses/Unauthorized" }
                    }
                },
                "post": {
                    "summary": "Add a thing",
                    "parameters": [
                        { "name": "dry_run", "in": "query", "required": true, "schema": { "type": "boolean" } },
                        { "name": "x-trace", "in": "header", "required": false, "schema": { "type": "string" } },
                        { "$ref": "#/components/parameters/Shared" }
                    ],
                    "responses": { "201": { "description": "Added" } }
                }
            },
            "/api/v1/things/{id}": {
                "get": {
                    "description": "   ",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "session", "in": "cookie", "required": false, "schema": { "type": "string" } },
                        { "name": "q", "in": "querystring", "required": false, "schema": { "type": "string" } }
                    ],
                    "responses": { "404": { "description": "No such thing" }, "200": { "description": "One thing" } }
                },
                "delete": {
                    "description": "Forget it",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "force", "in": "query", "required": false, "schema": { "type": "boolean" } }
                    ],
                    "responses": { "204": { "description": "Gone" } }
                },
                "put": { "responses": { "200": { "description": "Replaced" } } },
                "patch": { "responses": { "200": { "description": "Changed" } } },
                "head": { "responses": { "200": { "description": "There" } } },
                "options": { "responses": { "200": { "description": "Allowed" } } },
                "trace": { "responses": { "200": { "description": "Echoed" } } }
            },
            "/api/v1/things/{id}/sync": {
                "post": { "description": "Sync it", "responses": { "200": { "description": "Synced" } } },
                "put": { "responses": { "200": { "description": "Not an action" } } }
            },
            "/api/v1/things/{id}/parts": {
                "get": { "responses": { "200": { "description": "Its parts" } } }
            },
            "/api/v1/things/{id}/parts/{part}": {
                "post": { "responses": { "200": { "description": "Deeper than one segment" } } }
            },
            "/api/v1/things/{id}/{other}": {
                "post": { "responses": { "200": { "description": "A new capture" } } }
            },
            "/api/v1/run": {
                "post": { "responses": { "200": { "description": "POST only" } } }
            },
            "/api/v1/config/validate": {
                "post": { "responses": { "200": { "description": "Validated" } } }
            },
            "/api/v1/config/{file}": {
                "get": { "responses": { "200": { "description": "A file" } } }
            }
        },
        "components": {
            "parameters": { "Shared": { "name": "shared", "in": "query", "required": false, "schema": { "type": "string" } } },
            "responses": { "Unauthorized": { "description": "No key" } }
        }
    });
    Spec::new(serde_json::from_value(doc).expect("a valid document"))
}

#[test]
fn a_route_carries_what_the_spec_says_about_it() {
    let doc = spec().lookup("/api/v1/things", "GET").expect("a route");
    assert_eq!(doc.template, "/api/v1/things");
    assert_eq!(doc.methods, ["GET", "POST"]);
    assert_eq!(doc.description.as_deref(), Some("Every thing."));
    // A parameter given by reference is not inlined, so it is not listed
    assert_eq!(doc.parameters.len(), 1);
    let limit = &doc.parameters[0];
    assert_eq!(
        (limit.name.as_str(), limit.location, limit.required),
        ("limit", "query", false)
    );
    assert_eq!(limit.description.as_deref(), Some("At most this many"));
    // Only inline responses are listed, sorted by code
    assert_eq!(
        doc.responses,
        [("200".to_string(), "The things".to_string())]
    );
}

#[test]
fn a_summary_stands_in_for_a_missing_description() {
    let doc = spec().lookup("/api/v1/things", "POST").unwrap();
    assert_eq!(doc.description.as_deref(), Some("Add a thing"));
    let required: Vec<(&str, &str, bool)> = doc
        .parameters
        .iter()
        .map(|p| (p.name.as_str(), p.location, p.required))
        .collect();
    assert_eq!(
        required,
        [("dry_run", "query", true), ("x-trace", "header", false)]
    );
}

#[test]
fn every_location_and_every_method_is_named() {
    let doc = spec().lookup("/api/v1/things/abc", "GET").unwrap();
    // A blank description is no description
    assert_eq!(doc.description, None);
    let locations: Vec<&str> = doc.parameters.iter().map(|p| p.location).collect();
    assert_eq!(locations, ["path", "cookie", "querystring"]);
    // Responses are listed by code, whatever order the document has them in
    let codes: Vec<&str> = doc
        .responses
        .iter()
        .map(|(code, _)| code.as_str())
        .collect();
    assert_eq!(codes, ["200", "404"]);
    assert_eq!(
        doc.methods,
        ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "PUT", "TRACE"]
    );
}

#[test]
fn a_method_the_route_does_not_answer_still_names_the_route() {
    let doc = spec().lookup("/api/v1/run", "GET").unwrap();
    assert_eq!(doc.template, "/api/v1/run");
    assert_eq!(doc.methods, ["POST"]);
    assert!(doc.description.is_none() && doc.parameters.is_empty() && doc.responses.is_empty());
}

#[test]
fn templates_match_segment_by_segment() {
    let spec = spec();
    // A trailing slash is the same path
    assert!(spec.lookup("/api/v1/things/", "GET").is_some());
    // A capture needs a real segment
    assert!(spec.lookup("/api/v1/things//sync", "POST").is_none());
    // Neither shorter nor longer than the template
    assert!(spec.lookup("/api/v1", "GET").is_none());
    assert!(spec.lookup("/api/v1/things/a/b/c/d", "GET").is_none());
    assert!(spec.lookup("/api/v2/things", "GET").is_none());
}

// `{file}` in the config route takes the rest of the path, slashes included,
// and the static route beside it wins where both match.
#[test]
fn the_config_file_route_takes_the_rest_of_the_path() {
    let spec = spec();
    let doc = spec
        .lookup("/api/v1/config/sources/acme/a.yaml", "GET")
        .unwrap();
    assert_eq!(doc.template, "/api/v1/config/{file}");
    let doc = spec.lookup("/api/v1/config/validate", "POST").unwrap();
    assert_eq!(doc.template, "/api/v1/config/validate");
    assert!(spec.lookup("/api/v1/config/", "GET").is_none());
}

#[test]
fn only_get_routes_are_linkable_and_only_some_page() {
    let spec = spec();
    assert!(spec.is_gettable("/api/v1/things?limit=2"));
    assert!(!spec.is_gettable("/api/v1/run"));
    assert!(!spec.is_gettable("/nowhere"));
    assert!(spec.accepts_limit("/api/v1/things?limit=2"));
    assert!(!spec.accepts_limit("/api/v1/things/abc"));
    assert!(!spec.accepts_limit("/nowhere"));
}

// What a resource page offers: its own non-GET methods and a POST one segment
// below, never a child that needs a value the page does not have.
#[test]
fn a_resource_offers_its_writes_and_its_child_posts() {
    let actions = spec().actions("/api/v1/things/{id}");
    let offered: Vec<(&str, &str)> = actions
        .iter()
        .map(|a| (a.method, a.suffix.as_str()))
        .collect();
    assert_eq!(
        offered,
        [
            ("DELETE", ""),
            ("PATCH", ""),
            ("PUT", ""),
            ("TRACE", ""),
            ("POST", "/sync")
        ]
    );
    let delete = &actions[0];
    assert_eq!(delete.description.as_deref(), Some("Forget it"));
    // Only query parameters become form fields
    let fields: Vec<&str> = delete.parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(fields, ["force"]);
    assert_eq!(actions[4].description.as_deref(), Some("Sync it"));

    // The list itself offers its POST, described by its summary
    let list = spec().actions("/api/v1/things");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].description.as_deref(), Some("Add a thing"));
    assert!(spec().actions("/api/v1/things/{id}/parts").is_empty());
}
