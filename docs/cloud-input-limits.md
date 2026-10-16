# Original input and decoding limits

Last updated: 2026-10-17

`cloud.overview.inputLimits` reports the actual build's admission limits. Managed
import/adoption and Cloud Synchronization show the relevant limits before starting.
Byte, dimension, pixel and supported-format checks all apply independently.

| Boundary | Limit | Meaning |
| --- | --- | --- |
| Native original input / in-memory source | 512 MiB | Local file hooks and managed acquisition check length before reserving/reading the payload. |
| Native original rendering and shared photo descriptors | 128,000,000 pixels; 65,535 per edge | RAW admission counts the full sensor before crop/binning; raster admission uses stored dimensions. |
| Browser original rendering / ordinary local import | 32,000,000 pixels; 65,535 per edge | Oversized shared managed photos use published smart previews for ordinary preview rendering. |
| Browser blob transfer / resident-source admission | 512 MiB per blob and aggregate admitted payload | A transfer limit is not a promise that the image can be decoded. |
| Cloud blob wire / native streaming transport | 4 GiB per object | This protocol bound does not increase input or decoder limits. |

Reducing the output preview/export size does not avoid decoding the original sensor
samples. Raster codecs check dimensions before pixel decoding; RAW geometry is probed
before full development. Some vendor header probes unpack CFA samples to infer sensor
metadata. Internal codec corruption guards are broader than these product admission
limits and do not imply supported product capacity. Memory use also includes decoded
pixels and render intermediates; the limits are not a memory or latency guarantee.

Browser preview selection respects its pixel budget even after an original has been
cached. Explicit original-required operations continue to require originals and report
their limits. Original-quality offline pins also protect the smart preview when needed
for rendering; this includes whole-library offline selections. A missing required preview
keeps offline readiness false while retaining protection of the original. An explicit
`allowSmartPreview:true` rendered export may use
the preview and stays capped to its native size; full-quality export never silently
substitutes it. Publish a smart preview from desktop for large shared photos. Preserve
oversized originals and use smaller lossless derivatives when native admission fails.

All current devices must upgrade before sharing photos above the previous 100 MP
pre-release descriptor bound. The wire fields are unchanged; older builds reject those
photos. At this pre-release boundary the maintainer confirmed only disposable test
data (2026-10-14).

## Representative large RAW

The pinned CC0 raw.pixls.us sample is Hasselblad X2D 100C, uncompressed 16-bit `.3fr`,
213,311,488 bytes. Sensor dimensions are 11,904 × 8,842; default developed crop is
11,656 × 8,742 (over 100 MP). SHA-256:
`dcc5a4abe3498e6f25e89bb491995fd12c3b669d9277c71b45b49210d3e56280`.

The sample is in `xtask/src/main.rs`'s CC0 corpus manifest, downloaded only to ignored
`corpus/raw/`; no media is committed. Source: [raw.pixls.us sample 6565](https://raw.pixls.us/getfile.php/6565/nice/Hasselblad%20-%20X2D%20100C%20-%2016bit%20%284:3%29.3FR),
licensed [CC0](https://creativecommons.org/publicdomain/zero/1.0/).
Task-specific acceptance evidence belongs in [the reliability tracker](cloud-reliability-tasks.md).
This one input does not establish every Hasselblad compression variant, camera colour
parity or full-resolution export performance.

## Revision history

| Date | Change |
| --- | --- |
| 2026-10-17 | CR.19: shared admission/reporting and explicit native/browser decode budgets; pinned large 3FR corpus input. |
