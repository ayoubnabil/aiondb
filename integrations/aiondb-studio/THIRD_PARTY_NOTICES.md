# AionDB Studio Third-Party Notices

This file documents the third-party browser assets vendored under
`integrations/aiondb-studio/static/`.

The AionDB-specific Go and frontend changes in this subtree follow AionDB's
project license. The upstream pgweb code and the browser assets below keep
their original licenses and notices.

## pgweb

- Upstream: <https://github.com/sosedoff/pgweb>
- License: MIT
- Local license file: `integrations/aiondb-studio/LICENSE`

## Bundled browser assets

The following entries were verified from the vendored file headers currently in
this repository.

### jQuery

- Files: `static/js/jquery.js`
- Upstream notice in file: `jQuery v2.1.1`
- License: MIT

### Bootstrap

- Files:
  - `static/css/bootstrap.css`
  - `static/js/bootstrap-dropdown.js`
- Upstream notice in file: `Bootstrap v3.2.0`
- License: MIT
- Embedded upstream dependency noted in file: `normalize.css v3.0.1` under MIT

### Bootstrap Context Menu

- Files: `static/js/bootstrap-contextmenu.js`
- Upstream: <https://github.com/sydcanem/bootstrap-contextmenu>
- License: MIT

### Font Awesome

- Files:
  - `static/css/font-awesome.css`
  - `static/fonts/FontAwesome.otf`
  - `static/fonts/fontawesome-webfont.eot`
  - `static/fonts/fontawesome-webfont.svg`
  - `static/fonts/fontawesome-webfont.ttf`
  - `static/fonts/fontawesome-webfont.woff`
- Upstream notice in file: `Font Awesome 4.2.0`
- Licenses:
  - CSS: MIT
  - Fonts: SIL OFL 1.1

### Base64 helper

- Files: `static/js/base64.js`
- Upstream notice in file: `http://www.webtoolkit.info/`
- Note: the vendored file carries provenance comments but does not spell out a
  local license identifier. Keep it bundled with this notice set unless you
  separately re-verify and replace it with a freshly sourced copy.

### Ace editor

- Files:
  - `static/js/ace.js`
  - `static/js/ace-pgsql.js`
- Upstream: <https://github.com/ajaxorg/ace>
- License: BSD-style (the upstream Ace repository states `BSD License`)
- Note: the bundled build does not carry a prominent license banner at the top
  of `ace.js`, so this notice records the upstream license explicitly.

## Residual caution

`static/js/bootstrap3-typeahead.min.js` is a vendored third-party autocomplete
asset inherited from pgweb, but the minified file in this repository does not
carry a self-identifying license banner. Keep it bundled with this notice set
and the upstream pgweb attribution unless you separately re-verify its exact
upstream package provenance before extracting or relicensing it on its own.
