use crate::spec::Spec;

// What the pages need to know about the service they are part of. Fixed for
// the life of the process, so it is all `'static`: a service builds its Spec
// once and hands out the reference.
#[derive(Clone, Copy)]
pub struct Site {
    // The product name, in every page's title and header ("unified-api")
    pub product: &'static str,
    // The service's OpenAPI document, for each page's help, links and actions
    pub spec: &'static Spec,
    // The browser session cookie, as set by the sign-in form
    pub session_cookie: &'static str,
    // The cookie that pairs a sign-in form with the server that served it
    pub login_cookie: &'static str,
    // The cookie that remembers the theme a browser picked
    pub theme_cookie: &'static str,
    // Route templates whose body is the service's product rather than one of
    // its own resources (an output endpoint's inventory): a browser gets
    // their bytes as they are, never a page about them.
    pub raw_routes: &'static [&'static str],
}
