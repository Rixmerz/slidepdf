//! Fragments: one small, standalone PDF per slide.
//!
//! A fragment is a valid one-page PDF, so it can be stored on a CDN, cached
//! and shown on its own (a "slice"). The manifest lists them in order with
//! a content hash, size, title and text, which gives a viewer lazy loading
//! and a search index for free. [`assemble`] turns any list of PDFs (ours or
//! not) back into one document, deduplicating identical images and fonts so
//! the result is not bigger than a direct conversion. [`split`] does the
//! reverse for an arbitrary existing PDF.

use crate::pdf::fnv1a;

#[derive(Debug, Clone)]
pub struct Fragment {
    /// 1-based slide number.
    pub number: usize,
    pub file: String,
    pub pdf: Vec<u8>,
    pub title: Option<String>,
    pub text: String,
    pub warnings: Vec<String>,
}

impl Fragment {
    pub fn hash(&self) -> String {
        format!("{:016x}", fnv1a(&self.pdf))
    }

    /// The manifest line for this fragment (everything but the PDF bytes).
    pub fn entry(&self) -> Entry {
        Entry {
            number: self.number,
            file: self.file.clone(),
            bytes: self.pdf.len(),
            hash: self.hash(),
            title: self.title.clone(),
            text: self.text.clone(),
            warnings: self.warnings.clone(),
        }
    }
}

/// One slide in the manifest.
#[derive(Debug, Clone)]
pub struct Entry {
    pub number: usize,
    pub file: String,
    pub bytes: usize,
    pub hash: String,
    pub title: Option<String>,
    pub text: String,
    pub warnings: Vec<String>,
}

pub fn fragment_file_name(number: usize) -> String {
    format!("slide-{number:03}.pdf")
}

/// Index of a fragmented deck, serialized as `manifest.json`.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub source: String,
    /// Page size in PDF points.
    pub width: f32,
    pub height: f32,
    pub fragments: Vec<Entry>,
    pub pdf_file: Option<String>,
    pub pdf_bytes: Option<usize>,
    pub warnings: Vec<String>,
}

impl Manifest {
    pub fn to_json(&self) -> String {
        let mut o = String::from("{\n");
        o += "  \"version\": 1,\n";
        o += &format!("  \"source\": {},\n", js(&self.source));
        o += &format!("  \"width\": {:.2},\n  \"height\": {:.2},\n", self.width, self.height);
        if let (Some(f), Some(b)) = (&self.pdf_file, self.pdf_bytes) {
            o += &format!("  \"pdf\": {{ \"file\": {}, \"bytes\": {b} }},\n", js(f));
        }
        o += &format!("  \"count\": {},\n", self.fragments.len());
        o += &format!(
            "  \"warnings\": [{}],\n",
            self.warnings.iter().map(|w| js(w)).collect::<Vec<_>>().join(", ")
        );
        o += "  \"slides\": [\n";
        for (i, f) in self.fragments.iter().enumerate() {
            o += &format!(
                "    {{ \"number\": {}, \"file\": {}, \"bytes\": {}, \"hash\": {}, \"title\": {}, \"text\": {}, \"warnings\": [{}] }}{}\n",
                f.number,
                js(&f.file),
                f.bytes,
                js(&f.hash),
                f.title.as_deref().map(js).unwrap_or_else(|| "null".into()),
                js(&f.text),
                f.warnings.iter().map(|w| js(w)).collect::<Vec<_>>().join(", "),
                if i + 1 < self.fragments.len() { "," } else { "" }
            );
        }
        o += "  ]\n}\n";
        o
    }
}

/// JSON string literal.
pub fn js(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Reads the ordered `(file, hash)` list back from a manifest written by
/// [`Manifest::to_json`]. It is a narrow reader for our own format, not a
/// general JSON parser.
pub fn manifest_entries(json: &str) -> Vec<(String, String)> {
    let Some(start) = json.find("\"slides\"") else { return Vec::new() };
    let mut out = Vec::new();
    for line in json[start..].lines() {
        let field = |name: &str| -> Option<String> {
            let key = format!("\"{name}\": \"");
            let i = line.find(&key)? + key.len();
            let end = line[i..].find('"')?;
            Some(line[i..i + end].to_string())
        };
        if let (Some(f), Some(h)) = (field("file"), field("hash")) {
            out.push((f, h));
        }
    }
    out
}

/// Checks a stored fragment against the hash recorded in the manifest.
pub fn verify(pdf: &[u8], hash: &str) -> bool {
    format!("{:016x}", fnv1a(pdf)) == hash
}

#[cfg(feature = "assemble")]
pub use merge::{assemble, assemble_to, page_count, split, split_each, SplitInfo};

#[cfg(feature = "assemble")]
mod merge {
    //! Streaming merge and split over lopdf's parser. Objects are written
    //! out as soon as they are resolved, so memory is about one input
    //! fragment (assemble) or one input document (split), never the output.

    use std::collections::{BTreeSet, HashMap, HashSet};
    use std::io::Write;

    use lopdf::{Document, Object, ObjectId, StringFormat};

    use crate::pdf::fnv1a;

    const INHERITABLE: [&[u8]; 4] = [b"MediaBox", b"CropBox", b"Resources", b"Rotate"];

    fn type_of(o: &Object) -> Option<&[u8]> {
        let d = match o {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            _ => return None,
        };
        d.get(b"Type").ok().and_then(|t| t.as_name().ok())
    }

    /// Copies attributes a page inherits from its page-tree ancestors onto
    /// the page itself, so it survives being moved to a new tree.
    fn flatten_inherited(doc: &mut Document, page: ObjectId) {
        let mut parent = doc
            .get_dictionary(page)
            .ok()
            .and_then(|d| d.get(b"Parent").ok())
            .and_then(|p| p.as_reference().ok());
        let mut found: Vec<(Vec<u8>, Object)> = Vec::new();
        let mut guard = 0;
        while let Some(pid) = parent {
            guard += 1;
            if guard > 64 {
                break;
            }
            let Ok(pd) = doc.get_dictionary(pid) else { break };
            for k in INHERITABLE {
                if let Ok(v) = pd.get(k) {
                    if !found.iter().any(|(fk, _)| fk == k) {
                        found.push((k.to_vec(), v.clone()));
                    }
                }
            }
            parent = pd.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
        }
        if let Ok(d) = doc.get_dictionary_mut(page) {
            for (k, v) in found {
                if !d.has(&k) {
                    d.set(k, v);
                }
            }
        }
    }

    fn refs_in(o: &Object, out: &mut Vec<ObjectId>) {
        match o {
            Object::Reference(r) => out.push(*r),
            Object::Array(a) => a.iter().for_each(|x| refs_in(x, out)),
            Object::Dictionary(d) => d.iter().for_each(|(_, v)| refs_in(v, out)),
            Object::Stream(s) => s.dict.iter().for_each(|(_, v)| refs_in(v, out)),
            _ => {}
        }
    }

    /// Objects reachable from `roots`, not following page-tree parents.
    fn reachable(doc: &Document, roots: &[ObjectId]) -> BTreeSet<ObjectId> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<ObjectId> = Vec::new();
        for r in roots {
            if let Ok(o) = doc.get_object(*r) {
                let mut v = Vec::new();
                match o {
                    Object::Dictionary(d) => d
                        .iter()
                        .filter(|(k, _)| k.as_slice() != b"Parent")
                        .for_each(|(_, x)| refs_in(x, &mut v)),
                    other => refs_in(other, &mut v),
                }
                stack.extend(v);
            }
        }
        while let Some(id) = stack.pop() {
            if roots.contains(&id) || !seen.insert(id) {
                continue;
            }
            let Ok(o) = doc.get_object(id) else { continue };
            if matches!(type_of(o), Some(b"Pages") | Some(b"Catalog")) {
                seen.remove(&id);
                continue;
            }
            let mut v = Vec::new();
            refs_in(o, &mut v);
            stack.extend(v);
        }
        seen
    }

    fn rewrite(o: &Object, map: &HashMap<ObjectId, u32>) -> Object {
        match o {
            Object::Reference(r) => match map.get(r) {
                Some(n) => Object::Reference((*n, 0)),
                None => Object::Null,
            },
            Object::Array(a) => Object::Array(a.iter().map(|x| rewrite(x, map)).collect()),
            Object::Dictionary(d) => {
                let mut d = d.clone();
                for (_, v) in d.iter_mut() {
                    *v = rewrite(v, map);
                }
                Object::Dictionary(d)
            }
            Object::Stream(s) => {
                let mut s = s.clone();
                for (_, v) in s.dict.iter_mut() {
                    *v = rewrite(v, map);
                }
                Object::Stream(s)
            }
            other => other.clone(),
        }
    }

    // ---- serialization (lopdf keeps its writer private)

    fn ser(o: &Object, out: &mut Vec<u8>) {
        match o {
            Object::Null => out.extend_from_slice(b"null"),
            Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
            Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
            Object::Real(f) => {
                let s = format!("{f:.5}");
                let s = s.trim_end_matches('0').trim_end_matches('.');
                out.extend_from_slice(if s.is_empty() || s == "-" { b"0" } else { s.as_bytes() });
            }
            Object::Name(n) => {
                out.push(b'/');
                for &b in n {
                    if (33..=126).contains(&b) && !b"#()<>[]{}/%".contains(&b) {
                        out.push(b);
                    } else {
                        out.extend_from_slice(format!("#{b:02X}").as_bytes());
                    }
                }
            }
            Object::String(bytes, StringFormat::Hexadecimal) => {
                out.push(b'<');
                for b in bytes {
                    out.extend_from_slice(format!("{b:02X}").as_bytes());
                }
                out.push(b'>');
            }
            Object::String(bytes, _) => {
                out.push(b'(');
                for &b in bytes {
                    match b {
                        b'(' | b')' | b'\\' => {
                            out.push(b'\\');
                            out.push(b);
                        }
                        b'\r' => out.extend_from_slice(b"\\r"),
                        b'\n' => out.extend_from_slice(b"\\n"),
                        32..=126 => out.push(b),
                        _ => out.extend_from_slice(format!("\\{b:03o}").as_bytes()),
                    }
                }
                out.push(b')');
            }
            Object::Array(a) => {
                out.push(b'[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    ser(x, out);
                }
                out.push(b']');
            }
            Object::Dictionary(d) => {
                out.extend_from_slice(b"<<");
                for (k, v) in d.iter() {
                    ser(&Object::Name(k.clone()), out);
                    out.push(b' ');
                    ser(v, out);
                }
                out.extend_from_slice(b">>");
            }
            Object::Stream(s) => {
                let mut d = s.dict.clone();
                d.set("Length", s.content.len() as i64);
                ser(&Object::Dictionary(d), out);
                out.extend_from_slice(b"\nstream\n");
                out.extend_from_slice(&s.content);
                out.extend_from_slice(b"\nendstream");
            }
            Object::Reference((n, g)) => out.extend_from_slice(format!("{n} {g} R").as_bytes()),
        }
    }

    /// Writes indirect objects to a sink and a cross-reference stream at the end.
    struct ObjWriter<'w> {
        w: &'w mut dyn Write,
        pos: u64,
        offsets: Vec<(u32, u64)>,
        next: u32,
    }

    impl<'w> ObjWriter<'w> {
        fn new(w: &'w mut dyn Write) -> Result<Self, String> {
            let mut o = ObjWriter {
                w,
                pos: 0,
                offsets: Vec::new(),
                next: 1,
            };
            o.raw(b"%PDF-1.7\n%\x80\x80\x80\x80\n")?;
            Ok(o)
        }

        fn alloc(&mut self) -> u32 {
            self.next += 1;
            self.next - 1
        }

        fn raw(&mut self, b: &[u8]) -> Result<(), String> {
            self.w.write_all(b).map_err(|e| e.to_string())?;
            self.pos += b.len() as u64;
            Ok(())
        }

        fn obj(&mut self, id: u32, o: &Object) -> Result<(), String> {
            self.offsets.push((id, self.pos));
            let mut buf = format!("{id} 0 obj\n").into_bytes();
            ser(o, &mut buf);
            buf.extend_from_slice(b"\nendobj\n");
            self.raw(&buf)
        }

        fn finish(mut self, root: u32) -> Result<(), String> {
            let xref = self.alloc();
            let start = self.pos;
            if start > u32::MAX as u64 {
                return Err("PDF larger than 4 GB is not supported".into());
            }
            self.offsets.push((xref, start));
            let size = self.next as usize;
            let mut table = vec![0u8; size * 6];
            table[5] = 0xFF;
            for &(id, off) in &self.offsets {
                let e = &mut table[id as usize * 6..id as usize * 6 + 6];
                e[0] = 1;
                e[1..5].copy_from_slice(&(off as u32).to_be_bytes());
            }
            let packed = miniz_oxide::deflate::compress_to_vec_zlib(&table, 9);
            let head = format!(
                "{xref} 0 obj\n<</Type/XRef/Size {size}/W[1 4 1]/Root {root} 0 R/Filter/FlateDecode/Length {}>>\nstream\n",
                packed.len()
            );
            self.raw(head.as_bytes())?;
            self.raw(&packed)?;
            self.raw(format!("\nendstream\nendobj\nstartxref\n{start}\n%%EOF\n").as_bytes())?;
            self.w.flush().map_err(|e| e.to_string())
        }

        fn tree_and_catalog(&mut self, tree: u32, pages: &[u32]) -> Result<u32, String> {
            let kids: Vec<Object> = pages.iter().map(|&p| Object::Reference((p, 0))).collect();
            let mut t = lopdf::Dictionary::new();
            t.set("Type", Object::Name(b"Pages".to_vec()));
            t.set("Kids", Object::Array(kids));
            t.set("Count", pages.len() as i64);
            self.obj(tree, &Object::Dictionary(t))?;
            let catalog = self.alloc();
            let mut c = lopdf::Dictionary::new();
            c.set("Type", Object::Name(b"Catalog".to_vec()));
            c.set("Pages", Object::Reference((tree, 0)));
            self.obj(catalog, &Object::Dictionary(c))?;
            Ok(catalog)
        }
    }

    /// Fingerprint of an object after its references were rewritten to
    /// output ids, so equal content means equal fingerprint.
    fn fingerprint(o: &Object) -> (u64, usize) {
        let mut buf = Vec::new();
        ser(o, &mut buf);
        (fnv1a(&buf), buf.len())
    }

    /// Merges PDFs page by page into `out`, writing as it goes. Only one
    /// input is parsed at a time; identical objects (images, fonts, even
    /// content streams) are written once.
    pub fn assemble_to<I>(parts: I, out: &mut dyn Write) -> Result<usize, String>
    where
        I: IntoIterator<Item = Result<Vec<u8>, String>>,
    {
        let mut w = ObjWriter::new(out)?;
        let tree = w.alloc();
        let mut all_pages: Vec<u32> = Vec::new();
        let mut written: HashMap<(u64, usize), u32> = HashMap::new();

        for (n, part) in parts.into_iter().enumerate() {
            let bytes = part?;
            let mut doc = Document::load_mem(&bytes).map_err(|e| format!("input {}: not a readable PDF: {e}", n + 1))?;
            drop(bytes);
            let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
            check_pages(&doc, pages.len()).map_err(|e| format!("input {}: {e}", n + 1))?;
            for &p in &pages {
                flatten_inherited(&mut doc, p);
            }
            let mut map: HashMap<ObjectId, u32> = HashMap::new();
            for &p in &pages {
                let id = w.alloc();
                map.insert(p, id);
                all_pages.push(id);
            }
            // Resolve objects bottom-up so each one is fingerprinted with
            // its references already pointing at output ids.
            let mut pending: Vec<ObjectId> = reachable(&doc, &pages).into_iter().collect();
            while !pending.is_empty() {
                let before = pending.len();
                let mut rest = Vec::new();
                for id in pending {
                    let Ok(o) = doc.get_object(id) else { continue };
                    let mut refs = Vec::new();
                    refs_in(o, &mut refs);
                    if refs.iter().all(|r| map.contains_key(r) || doc.get_object(*r).is_err()) {
                        let r = rewrite(o, &map);
                        let fp = fingerprint(&r);
                        let gid = match written.get(&fp) {
                            Some(&g) => g,
                            None => {
                                let g = w.alloc();
                                w.obj(g, &r)?;
                                written.insert(fp, g);
                                g
                            }
                        };
                        map.insert(id, gid);
                    } else {
                        rest.push(id);
                    }
                }
                if rest.len() == before {
                    // Reference cycle: give everything left an id, then write.
                    for &id in &rest {
                        let g = w.alloc();
                        map.insert(id, g);
                    }
                    for &id in &rest {
                        if let Ok(o) = doc.get_object(id) {
                            w.obj(map[&id], &rewrite(o, &map))?;
                        }
                    }
                    break;
                }
                pending = rest;
            }
            for &p in &pages {
                let Ok(o) = doc.get_object(p) else { continue };
                let mut r = rewrite(o, &map);
                if let Object::Dictionary(d) = &mut r {
                    d.set("Parent", Object::Reference((tree, 0)));
                    d.remove(b"Annots"); // annotations may point at pages of other inputs
                }
                w.obj(map[&p], &r)?;
            }
        }
        let n = all_pages.len();
        let catalog = w.tree_and_catalog(tree, &all_pages)?;
        w.finish(catalog)?;
        Ok(n)
    }

    /// In-memory convenience over [`assemble_to`].
    pub fn assemble(pdfs: &[&[u8]]) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        assemble_to(pdfs.iter().map(|p| Ok(p.to_vec())), &mut out)?;
        Ok(out)
    }

    /// Splits any PDF into one standalone PDF per page, handing each to
    /// `f(page_number, pdf)` as soon as it is built. Each page copies only
    /// the objects it uses. Returns the page count.
    pub fn split_each(pdf: &[u8], mut f: impl FnMut(usize, Vec<u8>) -> Result<(), String>) -> Result<SplitInfo, String> {
        let mut doc = Document::load_mem(pdf).map_err(|e| format!("not a readable PDF: {e}"))?;
        let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
        check_pages(&doc, pages.len())?;
        for &p in &pages {
            flatten_inherited(&mut doc, p);
        }
        // Size of the first page, after inheritance (works with object streams).
        let (width, height) = doc
            .get_dictionary(pages[0])
            .ok()
            .and_then(|d| d.get(b"MediaBox").ok())
            .and_then(|b| b.as_array().ok())
            .and_then(|a| {
                let v: Vec<f32> = a
                    .iter()
                    .filter_map(|x| x.as_float().ok().or_else(|| x.as_i64().ok().map(|i| i as f32)))
                    .collect();
                (v.len() == 4).then(|| ((v[2] - v[0]).abs(), (v[3] - v[1]).abs()))
            })
            .unwrap_or((612.0, 792.0));
        for (i, &page) in pages.iter().enumerate() {
            let mut out = Vec::new();
            {
                let mut w = ObjWriter::new(&mut out)?;
                let tree = w.alloc();
                let mut map: HashMap<ObjectId, u32> = HashMap::new();
                let pid = w.alloc();
                map.insert(page, pid);
                let objs = reachable(&doc, &[page]);
                for &id in &objs {
                    let g = w.alloc();
                    map.insert(id, g);
                }
                let known: HashSet<ObjectId> = map.keys().copied().collect();
                for &id in &objs {
                    if let Ok(o) = doc.get_object(id) {
                        w.obj(map[&id], &rewrite(o, &map))?;
                    }
                }
                let _ = known;
                if let Ok(o) = doc.get_object(page) {
                    let mut r = rewrite(o, &map);
                    if let Object::Dictionary(d) = &mut r {
                        d.set("Parent", Object::Reference((tree, 0)));
                        d.remove(b"Annots");
                    }
                    w.obj(pid, &r)?;
                }
                let catalog = w.tree_and_catalog(tree, &[pid])?;
                w.finish(catalog)?;
            }
            f(i + 1, out)?;
        }
        Ok(SplitInfo {
            pages: pages.len(),
            width,
            height,
        })
    }

    /// What [`split_each`] found: page count and first page size in points.
    #[derive(Debug, Clone, Copy)]
    pub struct SplitInfo {
        pub pages: usize,
        pub width: f32,
        pub height: f32,
    }

    /// Encrypted PDFs that lopdf could not open with the empty password
    /// come back with no pages; say so instead of producing nothing.
    fn check_pages(doc: &Document, pages: usize) -> Result<(), String> {
        if pages > 0 {
            Ok(())
        } else if doc.is_encrypted() {
            Err("PDF is password protected".into())
        } else {
            Err("PDF has no pages".into())
        }
    }

    /// In-memory convenience over [`split_each`].
    pub fn split(pdf: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let mut out = Vec::new();
        split_each(pdf, |_, p| {
            out.push(p);
            Ok(())
        })?;
        Ok(out)
    }

    pub fn page_count(pdf: &[u8]) -> Result<usize, String> {
        Document::load_mem(pdf).map(|d| d.get_pages().len()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trip() {
        let f = Fragment {
            number: 1,
            file: fragment_file_name(1),
            pdf: b"%PDF".to_vec(),
            title: Some("T \"q\"".into()),
            text: "a\nb".into(),
            warnings: vec![],
        };
        let m = Manifest {
            source: "x.pptx".into(),
            width: 720.0,
            height: 540.0,
            fragments: vec![f.entry()],
            pdf_file: None,
            pdf_bytes: None,
            warnings: vec![],
        };
        let entries = manifest_entries(&m.to_json());
        assert_eq!(entries, vec![("slide-001.pdf".to_string(), f.hash())]);
        assert!(verify(&f.pdf, &entries[0].1));
    }
}
