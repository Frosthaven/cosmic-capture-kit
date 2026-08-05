---
# No `title` here on purpose: with none set, the browser tab on the site root
# reads "Cosmic Capture Kit" instead of repeating it twice. The nav label comes
# from site-src/.pages.
template: home.html
# Material prefers a page's own description over `site_description`, so this is
# what the landing page's single `<meta name="description">` carries. It is the
# same sentence the card, the social previews and the structured data show; the
# template holds the copy those three share.
description: "Cross-platform screen region, window, and monitor capture with support for translucent windows, image, video, voice, QR, barcodes, OCR text, and annotation."
hide:
  - navigation
  - toc
---

<!--
The visible landing page lives in `site-overrides/home.html`, which replaces
this page's content block. The front matter above is what selects it.

Do not add copy here expecting it to render. It will not. Edit the template
instead, and keep the styling in `site-src/assets/custom.css`.
-->
