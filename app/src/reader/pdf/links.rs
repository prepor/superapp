//! PDF link annotations in the rendered page's coordinate system.

use hayro::hayro_syntax::{Pdf, page::Page, object::{Array, Dict, Name, Object, Rect, String as PdfString}};

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Url(String),
    Page(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// Normalized left, top, right, bottom, after cropping and rotation.
    pub rect: [f64; 4],
    pub target: Target,
}

pub(super) fn of(pdf: &Pdf, page: &Page<'_>) -> Vec<Link> {
    let Some(annotations) = page.raw().get::<Array<'_>>(b"Annots") else { return Vec::new() };
    let root = pdf.xref().get::<Dict<'_>>(pdf.xref().root_id()).unwrap_or_default();
    annotations.iter::<Dict<'_>>().take(4096).filter_map(|annotation| {
        if annotation.get::<Name<'_>>(b"Subtype").as_deref() != Some(b"Link")
            || annotation.get::<i32>(b"F").unwrap_or(0) & 35 != 0 {
            return None;
        }
        let rect = bounds(page, annotation.get::<Rect>(b"Rect")?)?;
        let target = if let Some(dest) = annotation.get::<Object<'_>>(b"Dest") {
            destination(pdf, &root, dest, 0).map(Target::Page)
        } else {
            let action = annotation.get::<Dict<'_>>(b"A")?;
            match action.get::<Name<'_>>(b"S").as_deref()? {
                b"URI" => {
                    let uri = string(&action.get::<PdfString<'_>>(b"URI")?);
                    let base = root.get::<Dict<'_>>(b"URI")
                        .and_then(|d| d.get::<PdfString<'_>>(b"Base"));
                    external(&uri, base.as_ref().map(string).as_deref()).map(Target::Url)
                }
                b"GoTo" => destination(pdf, &root, action.get::<Object<'_>>(b"D")?, 0).map(Target::Page),
                _ => None,
            }
        }?;
        Some(Link { rect, target })
    }).collect()
}

fn bounds(page: &Page<'_>, rect: Rect) -> Option<[f64; 4]> {
    let [a, b, c, d, e, f] = page.initial_transform(true).as_coeffs();
    let (w, h) = page.render_dimensions();
    let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for (x, y) in [(rect.x0, rect.y0), (rect.x1, rect.y0), (rect.x0, rect.y1), (rect.x1, rect.y1)] {
        let (x, y) = ((a * x + c * y + e) / w as f64, (b * x + d * y + f) / h as f64);
        if !x.is_finite() || !y.is_finite() { return None; }
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x);
        bounds[3] = bounds[3].max(y);
    }
    let bounds = bounds.map(|n| n.clamp(0.0, 1.0));
    (bounds[2] > bounds[0] && bounds[3] > bounds[1]).then_some(bounds)
}

fn string(value: &PdfString<'_>) -> String {
    let bytes = value.as_bytes();
    if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        String::from_utf16_lossy(&bytes.as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes(*b)).collect::<Vec<_>>())
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn external(uri: &str, base: Option<&str>) -> Option<String> {
    let uri = uri.trim();
    if uri.chars().any(char::is_control) { return None; }
    let url = url::Url::parse(uri).ok().or_else(|| url::Url::parse(base?).ok()?.join(uri).ok())?;
    matches!(url.scheme(), "https" | "http" | "mailto").then(|| url.into())
}

fn destination(pdf: &Pdf, root: &Dict<'_>, dest: Object<'_>, depth: usize) -> Option<usize> {
    if depth > 32 { return None; }
    let name = match dest {
        Object::Array(array) => {
            let page = array.iter::<Object<'_>>().next()?.into_dict()?;
            let id = page.obj_id()?;
            return pdf.pages().iter().position(|p| p.raw().obj_id() == Some(id));
        }
        Object::Dict(dict) => return destination(pdf, root, dict.get::<Object<'_>>(b"D")?, depth + 1),
        Object::Name(name) => name.as_ref().to_vec(),
        Object::String(name) => name.as_bytes().to_vec(),
        _ => return None,
    };
    if let Some(dest) = root.get::<Dict<'_>>(b"Dests").and_then(|d| d.get::<Object<'_>>(&name)) {
        return destination(pdf, root, dest, depth + 1);
    }
    let tree = root.get::<Dict<'_>>(b"Names")?.get::<Dict<'_>>(b"Dests")?;
    let mut budget = 4096;
    destination(pdf, root, named(tree, &name, 0, &mut budget)?, depth + 1)
}

fn named<'a>(tree: Dict<'a>, name: &[u8], depth: usize, budget: &mut usize) -> Option<Object<'a>> {
    if depth > 32 || *budget == 0 { return None; }
    *budget -= 1;
    if let Some(entries) = tree.get::<Array<'_>>(b"Names") {
        let mut entries = entries.iter::<Object<'_>>();
        while *budget > 0 {
            let key = entries.next()?;
            let value = entries.next()?;
            *budget -= 1;
            if key.into_string().is_some_and(|key| key.as_bytes() == name) { return Some(value); }
        }
    }
    if let Some(kids) = tree.get::<Array<'_>>(b"Kids") {
        for kid in kids.iter::<Dict<'_>>() {
            if *budget == 0 { break; }
            if let Some(value) = named(kid, name, depth + 1, budget) { return Some(value); }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_links_resolve_named_pages_uris_and_rotated_return_links() {
        let pdf = Pdf::new(kernel::caps::demo::PDF.to_vec()).unwrap();
        let first = of(&pdf, &pdf.pages()[0]);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].target, Target::Page(1));
        assert_eq!(first[1].target, Target::Url("https://example.com/".into()));
        let second = of(&pdf, &pdf.pages()[1]);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].target, Target::Page(0));
        let expected = [451.0 / 595.0, 30.0 / 420.0, 471.0 / 595.0, 130.0 / 420.0];
        for (actual, expected) in second[0].rect.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    fn document(page: &str, annotation: &str, catalog: &str, names: &str) -> Pdf {
        use std::fmt::Write;
        let objects = [
            format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>"),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [10 20 410 620] /CropBox [20 30 400 610] /Annots [5 0 R] {page} >>"),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 595] >>".into(),
            format!("<< /Type /Annot /Subtype /Link /Rect [30 40 100 60] {annotation} >>"),
            names.into(),
        ];
        let mut text = "%PDF-1.4\n".to_string();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(text.len());
            writeln!(text, "{} 0 obj\n{object}\nendobj", i + 1).unwrap();
        }
        let xref = text.len();
        writeln!(text, "xref\n0 {}\n0000000000 65535 f ", objects.len() + 1).unwrap();
        for offset in offsets { writeln!(text, "{offset:010} 00000 n ").unwrap(); }
        write!(text, "trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").unwrap();
        Pdf::new(text.into_bytes()).unwrap()
    }

    #[test]
    fn links_follow_crop_and_every_rotation_and_ignore_hidden_actions() {
        for (rotation, expected) in [
            (0, [10.0 / 380.0, 550.0 / 580.0, 80.0 / 380.0, 570.0 / 580.0]),
            (90, [10.0 / 580.0, 10.0 / 380.0, 30.0 / 580.0, 80.0 / 380.0]),
            (180, [300.0 / 380.0, 10.0 / 580.0, 370.0 / 380.0, 30.0 / 580.0]),
            (270, [550.0 / 580.0, 300.0 / 380.0, 570.0 / 580.0, 370.0 / 380.0]),
        ] {
            let pdf = document(&format!("/Rotate {rotation}"), "/Dest [4 0 R /Fit]", "", "<<>>");
            let links = of(&pdf, &pdf.pages()[0]);
            assert_eq!(links[0].target, Target::Page(1));
            for (actual, expected) in links[0].rect.into_iter().zip(expected) {
                assert!((actual - expected).abs() < 1e-6, "rotation {rotation}: {actual} != {expected}");
            }
        }
        for annotation in ["/F 2 /Dest [4 0 R /Fit]", "/F 32 /Dest [4 0 R /Fit]",
            "/A << /S /Launch /F (program) >>", "/A << /S /URI /URI (javascript:alert) >>"] {
            let pdf = document("", annotation, "", "<<>>");
            assert!(of(&pdf, &pdf.pages()[0]).is_empty());
        }
    }

    #[test]
    fn destinations_support_name_trees_legacy_names_and_uri_bases() {
        for (annotation, catalog, tree, target) in [
            ("/Dest /back", "/Dests << /back [4 0 R /Fit] >>", "<<>>", Target::Page(1)),
            ("/A << /S /GoTo /D (there) >>", "/Names << /Dests << /Kids [6 0 R] >> >>",
                "<< /Names [(there) << /D [4 0 R /XYZ null null null] >>] >>", Target::Page(1)),
            ("/A << /S /URI /URI (../report) >>", "/URI << /Base (https://example.com/docs/) >>", "<<>>",
                Target::Url("https://example.com/report".into())),
        ] {
            let pdf = document("", annotation, catalog, tree);
            assert_eq!(of(&pdf, &pdf.pages()[0])[0].target, target);
        }
        let pdf = document("", "/Dest (loop)", "/Names << /Dests 6 0 R >>", "<< /Kids [6 0 R] >>");
        assert!(of(&pdf, &pdf.pages()[0]).is_empty());
    }

    #[test]
    fn external_links_accept_web_and_mail_destinations() {
        assert_eq!(external("../report", Some("https://example.com/docs/")), Some("https://example.com/report".into()));
        assert_eq!(external("mailto:hello@example.com", None), Some("mailto:hello@example.com".into()));
        for uri in ["javascript:alert(1)", "file:///etc/passwd", "data:text/html,hi", "https://exam\nple.com"] {
            assert_eq!(external(uri, None), None);
        }
    }
}
