use std::sync::Arc;

use pdfcraft_cos::{SaveOptions, write_full};

use super::*;

fn reopen(doc: &Document) -> Document {
    let bytes = write_full(doc, &SaveOptions::default()).unwrap();
    hayro_syntax::Pdf::new(bytes.clone()).expect("parses");
    Document::open(Arc::new(bytes)).unwrap()
}

fn pages(doc: &Document) -> Vec<Dict> {
    let pages = doc.get(doc.root().unwrap()).as_dict().unwrap().reference(b"Pages").unwrap();
    let kids = doc.get(pages).as_dict().unwrap().get(b"Kids").unwrap().as_array().unwrap().clone();
    kids.iter().map(|k| doc.resolve(k).as_dict().cloned().unwrap()).collect()
}

fn media(d: &Dict) -> Vec<f64> {
    d.get(b"MediaBox").unwrap().as_array().unwrap().iter().map(|o| o.as_f64().unwrap()).collect()
}

/// A tiny PNG: 4×2 RGBA with one transparent pixel, 144 dpi.
fn png_bytes(alpha: bool) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 4, 2);
        enc.set_color(if alpha { png::ColorType::Rgba } else { png::ColorType::Rgb });
        enc.set_depth(png::BitDepth::Eight);
        enc.set_pixel_dims(Some(png::PixelDimensions { xppu: 5669, yppu: 5669, unit: png::Unit::Meter }));
        let mut w = enc.write_header().unwrap();
        let n = if alpha { 4 } else { 3 };
        let mut data = vec![200u8; 8 * n];
        if alpha {
            data[3] = 0;
        }
        w.write_image_data(&data).unwrap();
    }
    out
}

/// A minimal baseline JPEG header (SOI, JFIF at 300 dpi, SOF0 3×2 RGB, EOI). Only the headers
/// matter: the data is embedded as is.
fn jpeg_bytes() -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    v.extend_from_slice(&[0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 1, 0x01, 0x2C, 0x01, 0x2C, 0, 0]);
    v.extend_from_slice(&[0xFF, 0xC0, 0, 17, 8, 0, 2, 0, 3, 3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

#[test]
fn blank_documents() {
    let doc = reopen(&blank(612.0, 792.0, 3).unwrap());
    assert_eq!(pages(&doc).len(), 3);
    assert_eq!(media(&pages(&doc)[0]), [0.0, 0.0, 612.0, 792.0]);
    assert!(blank(1.0, 792.0, 1).is_err() && blank(612.0, 792.0, 0).is_err());
}

#[test]
fn images_become_pages_at_their_resolution() {
    let doc =
        from_images(&[("photo.png".into(), png_bytes(true)), ("scan.jpg".into(), jpeg_bytes()), ("flat.png".into(), png_bytes(false))]).unwrap();
    let doc = reopen(&doc);
    let p = pages(&doc);
    assert_eq!(p.len(), 3);
    // 4×2 px at 144 dpi → 2×1 pt; 3×2 px at 300 dpi → 0.72×0.48 pt.
    let m0 = media(&p[0]);
    assert!((m0[2] - 2.0).abs() < 0.01 && (m0[3] - 1.0).abs() < 0.01, "{m0:?}");
    let m1 = media(&p[1]);
    assert!((m1[2] - 0.72).abs() < 0.01, "{m1:?}");
    let img = |d: &Dict| {
        let x = doc.resolve(d.get(b"Resources").unwrap());
        let xo = doc.resolve(x.as_dict().unwrap().get(b"XObject").unwrap());
        doc.resolve(xo.as_dict().unwrap().get(b"Im0").unwrap()).as_dict().cloned().unwrap()
    };
    assert!(img(&p[0]).contains(b"SMask"), "transparency is kept");
    assert!(!img(&p[2]).contains(b"SMask"));
    assert_eq!(img(&p[1]).name(b"Filter"), Some(&b"DCTDecode"[..]));
    assert_eq!(img(&p[1]).name(b"ColorSpace"), Some(&b"DeviceRGB"[..]));
    let title = doc.resolve(doc.trailer().get(b"Info").unwrap());
    assert_eq!(title.as_dict().unwrap().get(b"Title").unwrap().as_string().unwrap().to_text(), "photo");
    assert!(matches!(from_images(&[("x.gif".into(), b"GIF89a".to_vec())]), Err(CreateError::Image(..))));
}

#[test]
fn image_resolution_override_changes_size_without_resampling() {
    let images = [("photo.png".into(), png_bytes(true)), ("scan.jpg".into(), jpeg_bytes())];
    for (dpi, sizes) in [(72.0, [(4.0, 2.0), (3.0, 2.0)]), (300.0, [(0.96, 0.48), (0.72, 0.48)])] {
        let doc = reopen(&from_images_with_resolution(&images, ImageResolution::Dpi(dpi)).unwrap());
        for (page, size) in pages(&doc).iter().zip(sizes) {
            let m = media(page);
            assert!((m[2] - size.0).abs() < 0.001 && (m[3] - size.1).abs() < 0.001);
            let res = doc.resolve(page.get(b"Resources").unwrap());
            let xo = doc.resolve(res.as_dict().unwrap().get(b"XObject").unwrap());
            let img = doc.resolve(xo.as_dict().unwrap().get(b"Im0").unwrap());
            let Object::Stream(stream) = &*img else { panic!("expected an image stream") };
            if stream.dict.name(b"Filter") == Some(&b"DCTDecode"[..]) {
                assert_eq!(*stream.raw, jpeg_bytes());
            } else {
                assert_eq!(stream.dict.int(b"Width"), Some(4));
                assert_eq!(stream.dict.int(b"Height"), Some(2));
                assert!(stream.dict.contains(b"SMask"));
            }
        }
    }
    for dpi in [0.0, -1.0, f64::NAN, f64::INFINITY, 1201.0] {
        assert!(matches!(from_images_with_resolution(&images, ImageResolution::Dpi(dpi)), Err(CreateError::Invalid(_))));
    }
}

#[test]
fn text_is_wrapped_and_paginated() {
    let long: String = (0..200).map(|i| format!("Line {i} of a plain text file\n")).collect();
    let doc = reopen(&from_text("notes", &format!("{long}\u{c}After a form feed"), LETTER, 11.0).unwrap());
    let p = pages(&doc);
    assert!(p.len() >= 5, "{} pages", p.len());
    let content = |d: &Dict| {
        let c = doc.resolve(d.get(b"Contents").unwrap());
        let Object::Stream(s) = &*c else { panic!() };
        String::from_utf8_lossy(&s.decoded().unwrap()).into_owned()
    };
    assert!(content(&p[0]).contains("(Line 0 of a plain text file) Tj"));
    assert!(content(p.last().unwrap()).contains("(After a form feed) Tj"), "a form feed starts a page");
}

fn image_of(doc: &Document, page: usize) -> Dict {
    let p = &pages(doc)[page];
    let res = doc.resolve(p.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let xo = doc.resolve(res.get(b"XObject").unwrap()).as_dict().cloned().unwrap();
    match &*doc.resolve(xo.get(b"Im0").unwrap()) {
        Object::Stream(s) => s.dict.clone(),
        _ => panic!("not an image"),
    }
}

/// Contributor-original ICC header, generated in code rather than a binary fixture. The
/// importer preserves profile bytes; these tests do not exercise a colour-management engine.
fn icc_profile(space: &[u8; 4], length: usize) -> Vec<u8> {
    let mut profile = vec![0; length];
    profile[..4].copy_from_slice(&(length as u32).to_be_bytes());
    profile[8..12].copy_from_slice(&[4, 0x30, 0, 0]);
    profile[12..16].copy_from_slice(b"mntr");
    profile[16..20].copy_from_slice(space);
    profile[20..24].copy_from_slice(b"XYZ ");
    profile[36..40].copy_from_slice(b"acsp");
    profile
}

fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut segment = vec![0xFF, marker];
    segment.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    segment.extend_from_slice(payload);
    segment
}

fn icc_segment(sequence: u8, count: u8, payload: &[u8]) -> Vec<u8> {
    let mut data = b"ICC_PROFILE\0".to_vec();
    data.extend_from_slice(&[sequence, count]);
    data.extend_from_slice(payload);
    jpeg_segment(0xE2, &data)
}

fn jpeg_with_segments(components: u8, before_frame: &[Vec<u8>], after_frame: &[Vec<u8>]) -> Vec<u8> {
    let mut jpeg = vec![0xFF, 0xD8];
    for segment in before_frame {
        jpeg.extend_from_slice(segment);
    }
    let mut frame = vec![8, 0, 2, 0, 3, components];
    for component in 1..=components {
        frame.extend_from_slice(&[component, 0x11, 0]);
    }
    jpeg.extend_from_slice(&jpeg_segment(0xC0, &frame));
    for segment in after_frame {
        jpeg.extend_from_slice(segment);
    }
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    jpeg
}

fn image_stream_of(doc: &Document, page: usize) -> Stream {
    let page = &pages(doc)[page];
    let res = doc.resolve(page.get(b"Resources").unwrap());
    let xo = doc.resolve(res.as_dict().unwrap().get(b"XObject").unwrap());
    let image = doc.resolve(xo.as_dict().unwrap().get(b"Im0").unwrap());
    let Object::Stream(stream) = &*image else { panic!("expected an image stream") };
    stream.clone()
}

fn assert_jpeg_icc(jpeg: &[u8], profile: &[u8], components: i64, alternate: &[u8]) -> Dict {
    let doc = reopen(&from_images(&[("profile.jpg".into(), jpeg.to_vec())]).unwrap());
    assert_jpeg_icc_in_document(&doc, jpeg, profile, components, alternate)
}

fn assert_jpeg_icc_in_document(doc: &Document, jpeg: &[u8], profile: &[u8], components: i64, alternate: &[u8]) -> Dict {
    let image = image_stream_of(doc, 0);
    assert_eq!(image.raw.as_ref(), jpeg, "JPEG bytes must not be recompressed");
    assert_eq!(image.dict.name(b"Filter"), Some(&b"DCTDecode"[..]));
    let color_space = image.dict.get(b"ColorSpace").unwrap().as_array().unwrap();
    assert_eq!(color_space.len(), 2);
    assert_eq!(color_space[0].as_name(), Some(&b"ICCBased"[..]));
    assert!(color_space[1].as_ref().is_some(), "ICC profile must be an indirect stream");
    let icc = doc.resolve(&color_space[1]);
    let Object::Stream(icc) = &*icc else { panic!("expected an ICC profile stream") };
    assert_eq!(icc.dict.int(b"N"), Some(components));
    assert_eq!(icc.dict.name(b"Alternate"), Some(alternate));
    assert_eq!(icc.decoded().unwrap(), profile, "ICC bytes survive save and reopen");
    image.dict
}

#[test]
fn jpeg_icc_profiles_survive_save_and_reopen() {
    for (components, space, alternate) in [(1, *b"GRAY", &b"DeviceGray"[..]), (3, *b"RGB ", &b"DeviceRGB"[..]), (4, *b"CMYK", &b"DeviceCMYK"[..])] {
        let profile = icc_profile(&space, 132);
        let jpeg = jpeg_with_segments(components, &[icc_segment(1, 1, &profile)], &[]);
        let dict = assert_jpeg_icc(&jpeg, &profile, components.into(), alternate);
        assert!(!dict.contains(b"Decode"), "uninverted samples remain uninverted");
    }
}

#[test]
fn jpeg_icc_is_preserved_when_embedding_an_image_xobject() {
    for (components, space, alternate) in [(1, *b"GRAY", &b"DeviceGray"[..]), (3, *b"RGB ", &b"DeviceRGB"[..]), (4, *b"CMYK", &b"DeviceCMYK"[..])] {
        let profile = icc_profile(&space, 132);
        let jpeg = jpeg_with_segments(components, &[icc_segment(1, 1, &profile)], &[]);
        let mut doc = Document::new_empty();
        let (image, size) = image_xobject(&mut doc, "profile.jpg", &jpeg).unwrap();
        assert_eq!(size, (3.0, 2.0));
        let mut xobjects = Dict::new();
        xobjects.set(b"Im0".to_vec(), Object::Ref(image));
        let mut resources = Dict::new();
        resources.set(b"XObject".to_vec(), Object::Dict(xobjects));
        add_page(&mut doc, size.0, size.1, resources, None).unwrap();
        assert_jpeg_icc_in_document(&reopen(&doc), &jpeg, &profile, components.into(), alternate);
    }
}

#[test]
fn jpeg_icc_marker_fill_bytes_are_accepted() {
    let profile = icc_profile(b"RGB ", 132);
    let mut filled = vec![0xFF];
    filled.extend_from_slice(&icc_segment(1, 1, &profile));
    let jpeg = jpeg_with_segments(3, &[filled], &[]);
    assert_jpeg_icc(&jpeg, &profile, 3, b"DeviceRGB");
}

#[test]
fn jpeg_icc_scanning_stops_at_start_of_scan() {
    let profile = icc_profile(b"RGB ", 132);
    let mut jpeg = jpeg_with_segments(3, &[icc_segment(1, 1, &profile)], &[]);
    jpeg.truncate(jpeg.len() - 2);
    jpeg.extend_from_slice(&jpeg_segment(0xDA, &[3, 1, 0, 2, 0x11, 3, 0x11, 0, 63, 0]));
    // These entropy bytes resemble a duplicate ICC APP2 and an inverted-CMYK marker.
    // They are opaque DCT data, and must never change the image metadata.
    jpeg.extend_from_slice(&icc_segment(1, 1, &profile));
    jpeg.extend_from_slice(&jpeg_segment(0xEE, b"Adobe\0\x64\0\0\0\0\0"));
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    assert_jpeg_icc(&jpeg, &profile, 3, b"DeviceRGB");
}

#[test]
fn jpeg_rejects_invalid_segment_lengths_without_panicking() {
    for segment in [vec![0xFF, 0xE2, 0, 0], vec![0xFF, 0xE2, 0, 1], vec![0xFF, 0xE2, 0], vec![0xFF, 0xE2, 0, 16, 1]] {
        let mut jpeg = jpeg_with_segments(3, &[], &[]);
        jpeg.truncate(jpeg.len() - 2);
        jpeg.extend_from_slice(&segment);
        assert!(matches!(from_images(&[("truncated.jpg".into(), jpeg)]), Err(CreateError::Image(..))), "{segment:?}");
    }
}

#[test]
fn jpeg_icc_chunks_reassemble_out_of_order() {
    let mut profile = icc_profile(b"RGB ", 236_000);
    for (index, byte) in profile[132..].iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    let segments: Vec<Vec<u8>> = profile.chunks(60_000).enumerate().map(|(index, chunk)| icc_segment(index as u8 + 1, 4, chunk)).collect();
    let reversed: Vec<Vec<u8>> = segments.into_iter().rev().collect();
    let jpeg = jpeg_with_segments(3, &reversed, &[]);
    assert_jpeg_icc(&jpeg, &profile, 3, b"DeviceRGB");
}

#[test]
fn jpeg_icc_after_frame_header_and_cmyk_decode_are_preserved() {
    let profile = icc_profile(b"CMYK", 132);
    // APP14 is synthetic compatibility metadata, not an asset from an Adobe product.
    let app14 = jpeg_segment(0xEE, b"Adobe\0\x64\0\0\0\0\0");
    let jpeg = jpeg_with_segments(4, &[], &[icc_segment(1, 1, &profile), app14]);
    let dict = assert_jpeg_icc(&jpeg, &profile, 4, b"DeviceCMYK");
    let decode: Vec<i64> = dict.get(b"Decode").unwrap().as_array().unwrap().iter().map(|value| value.as_int().unwrap()).collect();
    assert_eq!(decode, [1, 0, 1, 0, 1, 0, 1, 0]);
}

#[test]
fn jpeg_without_icc_keeps_device_color_spaces() {
    for (components, alternate) in [(1, &b"DeviceGray"[..]), (3, &b"DeviceRGB"[..]), (4, &b"DeviceCMYK"[..])] {
        let jpeg = jpeg_with_segments(components, &[jpeg_segment(0xE2, b"unrelated APP2 metadata")], &[]);
        let doc = reopen(&from_images(&[("device.jpg".into(), jpeg.clone())]).unwrap());
        let image = image_stream_of(&doc, 0);
        assert_eq!(image.dict.name(b"ColorSpace"), Some(alternate));
        assert_eq!(image.raw.as_ref(), jpeg.as_slice());
    }
}

#[test]
fn jpeg_rejects_invalid_icc_chunks() {
    let profile = icc_profile(b"RGB ", 132);
    let invalid = [
        ("zero sequence", vec![icc_segment(0, 1, &profile)]),
        ("zero count", vec![icc_segment(1, 0, &profile)]),
        ("sequence beyond count", vec![icc_segment(2, 1, &profile)]),
        ("missing chunk", vec![icc_segment(1, 2, &profile)]),
        ("duplicate chunk", vec![icc_segment(1, 2, &profile[..66]), icc_segment(1, 2, &profile[66..])]),
        ("conflicting counts", vec![icc_segment(1, 2, &profile[..66]), icc_segment(2, 3, &profile[66..])]),
        ("missing counters", vec![jpeg_segment(0xE2, b"ICC_PROFILE\0")]),
        ("empty chunk", vec![icc_segment(1, 1, &[])]),
    ];
    for (case, segments) in invalid {
        let jpeg = jpeg_with_segments(3, &segments, &[]);
        assert!(matches!(from_images(&[("broken.jpg".into(), jpeg)]), Err(CreateError::Image(..))), "{case}");
    }
}

#[test]
fn jpeg_rejects_invalid_icc_profiles() {
    let profile = icc_profile(b"RGB ", 132);
    let mut bad_signature = profile.clone();
    bad_signature[36..40].copy_from_slice(b"nope");
    let mut short_size = profile.clone();
    short_size[..4].copy_from_slice(&131u32.to_be_bytes());
    let mut long_size = profile.clone();
    long_size[..4].copy_from_slice(&133u32.to_be_bytes());
    let invalid = [
        ("truncated header", profile[..40].to_vec()),
        ("bad signature", bad_signature),
        ("size shorter than data", short_size),
        ("size longer than data", long_size),
        ("unsupported space", icc_profile(b"Lab ", 132)),
        ("gray profile for RGB samples", icc_profile(b"GRAY", 132)),
        ("CMYK profile for RGB samples", icc_profile(b"CMYK", 132)),
    ];
    for (case, profile) in invalid {
        let jpeg = jpeg_with_segments(3, &[icc_segment(1, 1, &profile)], &[]);
        assert!(matches!(from_images(&[("broken.jpg".into(), jpeg)]), Err(CreateError::Image(..))), "{case}");
    }
}

#[test]
fn bmp_gif_and_multi_page_tiff_images() {
    use image::{ImageEncoder, Rgba, RgbaImage};
    // BMP: 3×2 opaque colour.
    let mut bmp = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut bmp).write_image(&[10, 20, 30].repeat(6), 3, 2, image::ExtendedColorType::Rgb8).unwrap();
    // GIF: 2×2 with a transparent pixel.
    let mut gif = Vec::new();
    {
        let mut img = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        let mut enc = image::codecs::gif::GifEncoder::new(&mut gif);
        enc.encode(img.as_raw(), 2, 2, image::ExtendedColorType::Rgba8).unwrap();
    }
    // TIFF: two pages, an 8-bit gray one at 144 dpi and a 1-bit one.
    let mut tif = std::io::Cursor::new(Vec::new());
    {
        let mut enc = tiff::encoder::TiffEncoder::new(&mut tif).unwrap();
        let mut im = enc.new_image::<tiff::encoder::colortype::Gray8>(4, 2).unwrap();
        im.resolution(tiff::tags::ResolutionUnit::Inch, tiff::encoder::Rational { n: 144, d: 1 });
        im.write_data(&[128; 8]).unwrap();
        enc.write_image::<tiff::encoder::colortype::Gray8>(2, 2, &[0, 255, 255, 0]).unwrap();
    }
    let doc = from_images(&[("a.bmp".into(), bmp), ("b.gif".into(), gif), ("scan.tif".into(), tif.into_inner())]).unwrap();
    let doc = reopen(&doc);
    assert_eq!(pages(&doc).len(), 4, "one page per BMP and GIF, two for the TIFF");
    assert_eq!(media(&pages(&doc)[0]), [0.0, 0.0, 3.0, 2.0]);
    assert_eq!(image_of(&doc, 0).name(b"ColorSpace"), Some(&b"DeviceRGB"[..]));
    assert!(image_of(&doc, 1).contains(b"SMask"), "GIF transparency");
    assert_eq!(media(&pages(&doc)[2]), [0.0, 0.0, 2.0, 1.0], "4×2 px at 144 dpi");
    assert_eq!(image_of(&doc, 2).name(b"ColorSpace"), Some(&b"DeviceGray"[..]));
    assert!(matches!(from_images(&[("x.webp".into(), b"RIFF0000WEBP".to_vec())]), Err(CreateError::Image(..))));
}

#[test]
fn images_export_as_jpeg_unchanged_and_others_as_png() {
    let doc = reopen(
        &from_images(&[("photo.png".into(), png_bytes(true)), ("scan.jpg".into(), jpeg_bytes()), ("flat.png".into(), png_bytes(false))]).unwrap(),
    );
    let out = extract_images(&doc, &[0, 1, 2], 0);
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    let kinds: Vec<(usize, &str, u32, u32)> = out.images.iter().map(|i| (i.page, i.extension, i.width, i.height)).collect();
    assert_eq!(kinds, [(0, "png", 4, 2), (1, "jpg", 3, 2), (2, "png", 4, 2)]);
    assert_eq!(out.images[1].data, jpeg_bytes(), "JPEG data is written as is");
    // The PNG round-trips the pixels, with the soft mask as alpha.
    let dec = png::Decoder::new(std::io::Cursor::new(out.images[0].data.clone()));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(&buf[..8], &[200, 200, 200, 0, 200, 200, 200, 200]);
    // Pages filter; small images can be left out.
    assert_eq!(extract_images(&doc, &[2], 0).images.len(), 1);
    assert!(extract_images(&doc, &[0, 1, 2], 3).images.iter().all(|i| i.width.min(i.height) >= 3));
}

#[test]
fn indexed_and_one_bit_images_decode() {
    let mut doc = from_images(&[("flat.png".into(), png_bytes(false))]).unwrap();
    // Replace the page's image with a 4×1 indexed image (two colours) and add a 1-bit mask.
    let page = pdfcraft_model::pages(&doc)[0].clone();
    let res = doc.resolve(page.dict.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let xo = doc.resolve(res.get(b"XObject").unwrap()).as_dict().cloned().unwrap();
    let (_, r) = xo.iter().next().map(|(k, v)| (k.clone(), v.as_ref().unwrap())).unwrap();
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Image"));
    d.set(b"Width".to_vec(), Object::Int(4));
    d.set(b"Height".to_vec(), Object::Int(1));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(1));
    d.set(
        b"ColorSpace".to_vec(),
        Object::Array(vec![
            Object::name("Indexed"),
            Object::name("DeviceRGB"),
            Object::Int(1),
            Object::String(PdfString::literal(vec![255, 0, 0, 0, 0, 255])),
        ]),
    );
    doc.set(r, Object::Stream(Stream::from_raw(d, vec![0b0101_0000])));
    let out = extract_images(&doc, &[0], 0);
    let dec = png::Decoder::new(std::io::Cursor::new(out.images[0].data.clone()));
    let mut rd = dec.read_info().unwrap();
    let mut buf = vec![0; rd.output_buffer_size().unwrap()];
    rd.next_frame(&mut buf).unwrap();
    assert_eq!(&buf[..12], &[255, 0, 0, 0, 0, 255, 255, 0, 0, 0, 0, 255]);
}

#[test]
fn jpeg_2000_images_are_embedded_as_is() {
    // A JP2 file: signature, ftyp, jp2h (ihdr 30 × 20, 3 components; resc 5906 px/m ≈ 150 dpi),
    // and a (stub) codestream.
    let bx = |ty: &[u8], payload: &[u8]| {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&20u32.to_be_bytes());
    ihdr.extend_from_slice(&30u32.to_be_bytes());
    ihdr.extend_from_slice(&[0, 3, 7, 7, 0, 0]);
    let mut resc = Vec::new();
    for _ in 0..2 {
        resc.extend_from_slice(&5906u16.to_be_bytes());
        resc.extend_from_slice(&1u16.to_be_bytes());
    }
    resc.extend_from_slice(&[0, 0]);
    let jp2h = [bx(b"ihdr", &ihdr), bx(b"res ", &bx(b"resc", &resc))].concat();
    let file = [JP2_SIGNATURE.to_vec(), bx(b"ftyp", b"jp2 \0\0\0\0jp2 "), bx(b"jp2h", &jp2h), bx(b"jp2c", &[0xFF, 0x4F, 0xFF, 0x51])].concat();
    let doc = reopen(&from_images(&[("photo.jp2".into(), file.clone())]).unwrap());
    let p = &pages(&doc)[0];
    let m = media(p);
    assert!((m[2] - 30.0 * 72.0 / 150.0).abs() < 0.2 && (m[3] - 20.0 * 72.0 / 150.0).abs() < 0.2, "{m:?}");
    let out = extract_images(&doc, &[0], 0);
    assert!(out.images.is_empty() && out.skipped.len() == 1, "JPX can't be exported yet: {:?}", out.skipped);
    // A raw codestream: the size comes from SIZ.
    let mut siz = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
    for v in [64u32, 48, 0, 0] {
        siz.extend_from_slice(&v.to_be_bytes());
    }
    let doc = reopen(&from_images(&[("raw.j2k".into(), siz)]).unwrap());
    assert_eq!(media(&pages(&doc)[0])[2], 64.0);
}

#[test]
fn source_kind_tells_pdfs_images_and_text_apart() {
    assert_eq!(source_kind("a.bin", b"%PDF-1.7\n"), Some(SourceKind::Pdf));
    assert_eq!(source_kind("a.txt", b"junk before\n%PDF-1.4"), Some(SourceKind::Pdf), "a header after leading junk");
    assert_eq!(source_kind("a", &png_bytes(false)), Some(SourceKind::Image));
    assert_eq!(source_kind("a.txt", &jpeg_bytes()), Some(SourceKind::Image), "bytes win over the name");
    assert_eq!(source_kind("notes.TXT", b"hello"), Some(SourceKind::Text));
    assert_eq!(source_kind("notes.text", b""), Some(SourceKind::Text));
    // Text that merely starts like a BMP stays text; a truncated BMP header is not an image.
    assert_eq!(source_kind("b.txt", b"BMW drivers"), Some(SourceKind::Text));
    assert_eq!(source_kind("b.bmp", b"BM"), None);
    assert_eq!(source_kind("a.docx", b"PK\x03\x04"), None);
    assert_eq!(source_kind("", b""), None);
}
