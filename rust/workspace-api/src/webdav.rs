/*
 * Broodlink workspace-api — minimal WebDAV/CalDAV/CardDAV client helpers.
 * Enough to run REPORT/PROPFIND with Basic auth and pull calendar-data /
 * address-data blocks out of a multistatus response. The XML extraction is a
 * pure function (unit-tested); the network call is a thin reqwest wrapper.
 */

/// Unescape the XML entities that appear in CalDAV/CardDAV payloads.
pub fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#13;", "\r")
        .replace("&#10;", "\n")
        .replace("&amp;", "&")
}

/// Extract the inner text of every element whose *local* name matches
/// `local_name` (ignoring any namespace prefix), XML-unescaped. Used to pull
/// `calendar-data` / `address-data` blocks out of a DAV multistatus response.
pub fn extract_data_blocks(xml: &str, local_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let close_tag = format!("{local_name}>"); // matches the closing </…local_name>
    let mut i = 0;
    while let Some(rel) = xml[i..].find(local_name) {
        let at = i + rel;
        // Must look like a tag name: preceded by '<' or a namespace ':' that
        // itself follows '<'. Find the end of this opening tag.
        let prev = xml[..at].chars().last();
        if !matches!(prev, Some('<') | Some(':')) {
            i = at + local_name.len();
            continue;
        }
        let gt = match xml[at..].find('>') {
            Some(g) => at + g + 1,
            None => break,
        };
        // Find the closing tag of the same local name after the content.
        let close_rel = match xml[gt..].find(&format!("</")) {
            Some(_) => xml[gt..].find(&close_tag),
            None => None,
        };
        match close_rel {
            Some(cr) => {
                let close_abs = gt + cr;
                // back up to the "</" that begins the closing tag
                let content_end = xml[gt..close_abs]
                    .rfind("</")
                    .map(|p| gt + p)
                    .unwrap_or(close_abs);
                out.push(xml_unescape(xml[gt..content_end].trim()));
                i = close_abs + close_tag.len();
            }
            None => break,
        }
    }
    out
}

/// Run a CalDAV/CardDAV REPORT with Basic auth, returning the response body.
pub async fn report(
    url: &str,
    user: &str,
    pass: &str,
    depth: &str,
    body: &'static str,
) -> Result<String, String> {
    let method = reqwest::Method::from_bytes(b"REPORT").map_err(|e| e.to_string())?;
    let client = reqwest::Client::new();
    let resp = client
        .request(method, url)
        .basic_auth(user, Some(pass))
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("server returned {}", resp.status()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

/// PROPFIND for connectivity testing (accepts 200/207).
pub async fn propfind_ok(url: &str, user: &str, pass: &str) -> Result<(), String> {
    let method = reqwest::Method::from_bytes(b"PROPFIND").map_err(|e| e.to_string())?;
    let client = reqwest::Client::new();
    let resp = client
        .request(method, url)
        .basic_auth(user, Some(pass))
        .header("Depth", "0")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:displayname/></d:prop></d:propfind>"#)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let code = resp.status().as_u16();
    if code == 200 || code == 207 {
        Ok(())
    } else {
        Err(format!("server returned {code}"))
    }
}

pub const CALENDAR_QUERY: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><C:calendar-data/></D:prop>
  <C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT"/></C:comp-filter></C:filter>
</C:calendar-query>"#;

pub const ADDRESSBOOK_QUERY: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop><C:address-data/></D:prop>
</C:addressbook-query>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_namespaced_blocks() {
        let xml = r#"<multistatus xmlns:C="urn:caldav">
          <response><propstat><prop>
            <C:calendar-data>BEGIN:VCALENDAR&#13;
SUMMARY:A&#13;
END:VCALENDAR</C:calendar-data>
          </prop></propstat></response>
          <response><propstat><prop>
            <C:calendar-data>BEGIN:VCALENDAR
SUMMARY:B &amp; C
END:VCALENDAR</C:calendar-data>
          </prop></propstat></response>
        </multistatus>"#;
        let blocks = extract_data_blocks(xml, "calendar-data");
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("SUMMARY:A"));
        assert!(blocks[0].contains('\r')); // &#13; unescaped
        assert!(blocks[1].contains("SUMMARY:B & C")); // &amp; unescaped
    }

    #[test]
    fn handles_no_blocks() {
        assert!(extract_data_blocks("<multistatus/>", "address-data").is_empty());
    }
}
