use std::{fs::File, path::Path};
use tiny_http::{Header, Method, Request, Response, ResponseBox};

pub fn asset(root: &Path, request: &Request) -> Option<ResponseBox> {
    if !matches!(request.method(), Method::Get | Method::Head) {
        return None;
    }
    let path = request.url().split('?').next()?;
    let (file, mime) = if path == "/"
        || path
            .strip_prefix("/issues/")
            .is_some_and(protocol::valid_id)
    {
        (root.join("index.html"), "text/html; charset=utf-8")
    } else {
        let name = path.strip_prefix("/assets/")?;
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return None;
        }
        let mime = match Path::new(name).extension().and_then(|part| part.to_str())? {
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            _ => return None,
        };
        (root.join("assets").join(name), mime)
    };

    Some(match File::open(file) {
        Ok(file) => Response::from_file(file)
            .with_header(Header::from_bytes("Content-Type", mime).expect("static content type"))
            .boxed(),
        Err(_) => Response::empty(404).boxed(),
    })
}
