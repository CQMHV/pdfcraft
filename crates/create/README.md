# pdfcraft-create

Layer L4. Create a PDF (Acrobat's Create a PDF tool), execution plan M10.2:

- `blank(width, height, pages)`: an empty document;
- `from_images(&[(name, bytes)])`: one page per image, sized from the image's resolution
  (PNG `pHYs`, JPEG JFIF density; 72 dpi when absent). JPEG data is embedded as is
  (`/DCTDecode`, grey, RGB or Adobe-inverted CMYK); PNG is decoded and stored with Flate,
  with transparency as a soft mask;
- `from_images_with_resolution(images, ImageResolution::Dpi(dpi))`: override the page
  resolution (1–1200 dpi), without resampling or recompressing image pixels. Use 72 dpi
  for one point per pixel, or `ImageResolution::Embedded` for the same behavior as
  `from_images`. The largest page side is still capped at 14,400 points;
- `from_text(text, …)`: plain text set in Helvetica, wrapped and paginated.

Every function returns a `pdfcraft_cos::Document`; the caller writes it.

JPEG ICC profiles are reassembled from APP2 `ICC_PROFILE` segments and preserved byte for
byte in an indirect `/ICCBased` stream, with `/N` 1, 3 or 4 and the matching device-space
`/Alternate`. This applies both to image-page creation and `image_xobject` (added images and
stamps). JPEG samples, resolution and CMYK inversion are unchanged. Images without a profile
keep their device colour space. Missing, duplicate or inconsistent chunks, invalid profile
headers/sizes and colour spaces that do not match the JPEG components produce an image error
instead of silently discarding the profile. Header validation is not full ICC conformance
validation. Other image formats' ICC metadata is not handled by this change.
