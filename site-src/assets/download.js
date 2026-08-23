/*
 * The landing page's platform-aware download button, plus the download
 * analytics events (DRAGON-704).
 *
 * PROGRESSIVE ENHANCEMENT, and that word is load bearing. The button in
 * `site-overrides/home.html` ships as a plain link to the releases page
 * labelled "Download", and that markup is the FALLBACK, not a placeholder.
 * With no JavaScript, an unknown operating system, an architecture the rules
 * below refuse to guess at, a failed or rate-limited API call, or an artifact
 * that does not exist for the visitor's machine, the page keeps exactly that
 * link and exactly that label. This file only ever UPGRADES a working button;
 * it never breaks one.
 *
 * WHY THE GITHUB API AND NOT A DIRECT LINK. The obvious route,
 * `releases/latest/download/<name>`, cannot be used: the file name carries the
 * version (`CosmicCaptureKit-0.33.0-x86_64.msi`) so it cannot be written down
 * here without going stale on the next release, and github.com's HTML download
 * endpoints send no CORS headers, so the page cannot even ask them what exists.
 * `api.github.com` DOES send `access-control-allow-origin: *`, so one anonymous
 * GET of the latest release gives both the real file names and their download
 * URLs. Nothing here hardcodes a version. The API allows 60 anonymous requests
 * per hour per IP; over that it answers 403, which lands on the fallback like
 * any other failure.
 *
 * WHY THE MAC ARCHITECTURE IS NEVER GUESSED. Safari on Apple silicon reports
 * an INTEL user agent string, so a user agent alone can never decide between
 * the Apple silicon and the Intel build of macOS. Getting it wrong hands a
 * visitor a disk image their machine cannot open. So the rule is absolute:
 * on macOS the architecture comes from `getHighEntropyValues` (Chromium
 * only), or from a WebGL renderer name that POSITIVELY says Apple silicon,
 * or it stays unknown and the button falls back. An Intel-looking renderer
 * name proves nothing and is never accepted as evidence. The full set of
 * per-platform architecture rules, and why they are not symmetric, is written
 * out over `detectArch` below.
 *
 * THE PLATFORM LOGO follows the same rule as everything else here. The three
 * brand icons are already in the markup, hidden, and this file only ever
 * REVEALS the one it resolved. So an icon on that button is proof that
 * detection succeeded, and the fallback button carries none. The template's
 * `data-cck-os` values are the same three strings `detectOs` returns, which is
 * what keeps the icon and the link describing one platform and not two.
 *
 * The pure decisions (`detectPlatform`, `pickAsset`, and the three that read
 * the event's `arch`, `kind` and `version` off the chosen asset and the release
 * tag) take a plain object of hints, a plain list of assets and plain strings,
 * so they can be driven from node with fixtures and never touch the DOM. The
 * effectful parts (reading `navigator`, creating a WebGL context, fetching,
 * rewriting the button) are kept apart from them.
 */

(function () {
  "use strict";

  /*
   * The artifacts each (operating system, architecture) pair will accept, best
   * first, matched against the END of the release asset's file name. Every one
   * of these is a STABLE suffix: the version sits in the middle of the msi and
   * dmg names and the AppImages carry no version at all, so a suffix match is
   * version proof.
   *
   * Two of the six pairs have no asset in the current release: macOS on Intel,
   * and Windows on ARM64. They are listed anyway, because listing them is how a
   * missing artifact resolves cleanly rather than by guesswork, and the day
   * either one starts shipping it lights up here with no code change.
   *
   * Windows on ARM is the ONE entry with a second choice, and it is not an
   * inconsistency. Windows 11 on ARM runs x64 installers under emulation, so
   * the x86_64 msi is a genuinely working answer there, merely the second best
   * one. No other pair may grow a second choice: an AppImage or a disk image
   * for the wrong architecture simply does not run.
   */
  var ASSET_CANDIDATES = {
    "windows/x86_64": ["-x86_64.msi"],
    "windows/aarch64": ["-aarch64.msi", "-x86_64.msi"],
    "macos/aarch64": ["-aarch64.dmg"],
    "macos/x86_64": ["-x86_64.dmg"],
    "linux/x86_64": ["CosmicCaptureKit-x86_64.AppImage"],
    "linux/aarch64": ["CosmicCaptureKit-aarch64.AppImage"]
  };

  /*
   * What the upgraded button says. Linux says "COSMIC" rather than the
   * architecture because the desktop is the thing a reader has to check;
   * macOS says the architecture because that is the thing a reader has to
   * check there. A pair missing from this table can never be upgraded.
   */
  var LABELS = {
    "windows/x86_64": "Download for Windows",
    "windows/aarch64": "Download for Windows",
    "macos/aarch64": "Download for macOS (Apple Silicon)",
    "macos/x86_64": "Download for macOS (Intel)",
    "linux/x86_64": "Download for Linux (COSMIC)",
    "linux/aarch64": "Download for Linux (COSMIC)"
  };

  /*
   * The two analytics event names, and the ONLY two this site sends.
   *
   *   download                a resolved artifact link was pressed. Carries
   *                           four parameters (DRAGON-707), every one of them
   *                           derived in `upgrade`, where the link itself is
   *                           built, so an event can never describe a different
   *                           artifact than the one that was pressed:
   *
   *                             file     the exact release asset name, for
   *                                      example
   *                                      "CosmicCaptureKit-0.33.0-x86_64.msi".
   *                             arch     "x86_64" or "aarch64", read off the
   *                                      asset that was OFFERED, never off the
   *                                      machine that was detected. The two
   *                                      really can differ: an ARM Windows
   *                                      machine is offered the x86_64 msi when
   *                                      the release carries no aarch64 one,
   *                                      and the parameter has to say what was
   *                                      downloaded rather than what was
   *                                      wanted.
   *                             kind     "dmg", "msi", "appimage" or "zip",
   *                                      from the asset's extension. Linux is
   *                                      why it is worth counting on its own:
   *                                      it is the one platform shipping two
   *                                      kinds of the same build.
   *                             version  the release's own `tag_name` with a
   *                                      leading "v" removed, and NOT parsed
   *                                      out of the file name. The AppImages
   *                                      carry no version in their names at
   *                                      all, so a name-derived version would
   *                                      go missing for exactly the artifact
   *                                      Linux visitors are steered to.
   *
   *                           A parameter this file cannot determine is left
   *                           OUT of the event rather than sent as a guess or
   *                           as an empty string, so an absent key reads as
   *                           "unknown" and can never be mistaken for a value.
   *   download_from_releases  the fallback Download button or the All downloads
   *                           link was pressed, so the visitor went to the
   *                           releases page to choose for themselves. No
   *                           parameters, and it stays that way: there is no
   *                           file to name yet, so there is no architecture,
   *                           kind or version to name either.
   *
   * All four parameters describe the ARTIFACT and none of them describe the
   * visitor, which is what keeps the privacy page's "the download links you
   * press" an accurate description of the whole event.
   *
   * Both names are declared in the template's own comment too, so a reader of
   * the page markup sees the same two without opening this file.
   */
  var DIRECT_EVENT = "download";
  var RELEASES_EVENT = "download_from_releases";

  /*
   * How long to wait for the release API before giving up. The static button is
   * already correct, so a slow answer costs nothing except the risk of the
   * label changing under a reader who has already decided to click. Bounding
   * the request keeps that window short.
   */
  var REQUEST_BUDGET_MS = 4000;

  /* ---------------------------------------------------------------- */
  /* Pure decisions                                                    */
  /* ---------------------------------------------------------------- */

  /*
   * The operating system, from the Chromium `userAgentData.platform` value
   * when the browser has one, and from the user agent string otherwise.
   *
   * Returns "windows", "macos", "linux", or null for everything else, and
   * "everything else" genuinely means "no download for you here": Android,
   * iOS and ChromeOS all get the fallback button, since none of them can run
   * any artifact this project ships.
   *
   * Order matters in the string arm. An Android user agent contains the word
   * "Linux", and an iPad user agent contains the word "Macintosh", so both
   * have to be ruled out before the platform words are read.
   */
  function detectOs(userAgent, uaPlatform) {
    var named = String(uaPlatform || "").toLowerCase();
    if (named) {
      if (named === "windows") {
        return "windows";
      }
      if (named === "macos") {
        return "macos";
      }
      if (named === "linux") {
        return "linux";
      }
      return null;
    }

    var low = String(userAgent || "").toLowerCase();
    if (/iphone|ipad|ipod/.test(low)) {
      return null;
    }
    if (low.indexOf("android") !== -1) {
      return null;
    }
    if (/cros|chrome os|chromium os/.test(low)) {
      return null;
    }
    if (/windows|win32|win64/.test(low)) {
      return "windows";
    }
    if (/mac os x|macintosh/.test(low)) {
      return "macos";
    }
    if (/linux|x11/.test(low)) {
      return "linux";
    }
    return null;
  }

  /*
   * A machine the browser positively identified as 32 bit. Not an architecture,
   * a REFUSAL, and it has to be told apart from "unknown" because the two lead
   * to opposite places: unknown may take a safe per-platform default, while a
   * known 32 bit machine must take none, since this project ships no 32 bit
   * build for any platform.
   */
  var UNSUPPORTED_32 = "32-bit";

  /*
   * The architecture as the browser DECLARES it, from
   * `getHighEntropyValues(["architecture", "bitness"])`. Chromium answers
   * "x86" or "arm" with a bitness of "64" or "32". A missing bitness is
   * tolerated and read as 64, since that is what the entire supported world is;
   * only a bitness that positively says otherwise is the refusal above.
   */
  function archFromHints(architecture, bitness) {
    var arch = String(architecture || "").toLowerCase();
    var bits = String(bitness || "");
    if (!arch) {
      return null;
    }
    if (bits && bits !== "64") {
      return UNSUPPORTED_32;
    }
    if (arch === "x86") {
      return "x86_64";
    }
    if (arch === "arm") {
      return "aarch64";
    }
    return null;
  }

  /*
   * The architecture as the user agent STRING carries it. Used on Windows and
   * Linux only, never on macOS (see the mac rule in the file header). Linux
   * user agents do carry the machine: "X11; Linux x86_64" and
   * "X11; Linux aarch64" are both ordinary. "WOW64" and "Win64" both mean a 64
   * bit Windows, whatever the browser build is.
   */
  function archFromUserAgent(userAgent) {
    var low = String(userAgent || "").toLowerCase();
    if (/aarch64|arm64|armv8/.test(low)) {
      return "aarch64";
    }
    if (/x86_64|amd64|wow64|win64|x64/.test(low)) {
      return "x86_64";
    }
    return null;
  }

  /*
   * The architecture of a Mac, from its GPU name, and ONLY when that name
   * positively says Apple silicon. Every Apple silicon Mac has an Apple GPU
   * and no Intel Mac does, so "Apple M1", "Apple M4 Pro" and the generic
   * "Apple GPU" that Safari reports are all sound evidence, and they survive
   * Rosetta, because the GPU is hardware and does not get emulated.
   *
   * The reverse is NOT accepted. An Intel or AMD renderer name returns null
   * rather than "x86_64", per the never-guess rule: the Intel disk image does
   * not exist today, so claiming Intel could only ever produce a broken link
   * where the fallback would have worked.
   */
  function archFromGpu(gpu) {
    var name = String(gpu || "");
    if (!name) {
      return null;
    }
    if (/apple\s*(m\d|gpu|silicon)/i.test(name)) {
      return "aarch64";
    }
    return null;
  }

  /*
   * The architecture, and the ONE place the per-platform rules live. They are
   * deliberately NOT symmetric, because what a wrong answer costs is not
   * symmetric either.
   *
   *   Windows, positively ARM
   *     The aarch64 msi, and the x86_64 msi when the release carries no aarch64
   *     one, which is the case today. Windows 11 on ARM runs x64 installers
   *     under emulation, so the second choice really works. That fallback lives
   *     in the candidate table above, not here.
   *
   *   Windows, unknown or x86
   *     The x86_64 msi. Defaulting is SAFE in this one direction only: an ARM64
   *     device emulates x64 and copes, while an aarch64 msi on an x86 machine is
   *     a brick. So the aarch64 msi is never offered without a positive ARM
   *     reading, and the x86_64 msi is the standing default.
   *
   *   Linux
   *     Trust the user agent, which normally carries the real machine
   *     ("X11; Linux x86_64", "X11; Linux aarch64"). When it genuinely carries
   *     nothing, there is no default: Linux has no emulation layer to save an
   *     ARM device handed an x86_64 AppImage, so an absent architecture means
   *     the generic releases page, not a guess.
   *
   *   macOS
   *     Never a guess, for the reason in the file header. An aarch64 disk image
   *     cannot run on an Intel Mac, and routing Apple silicon to the Intel
   *     build would be a silent downgrade to Rosetta. Unknown means the generic
   *     releases page.
   *
   * A machine positively reported as 32 bit returns null on every platform,
   * INCLUDING Windows, so the Windows default cannot rescue it. There is no 32
   * bit build to rescue it with.
   */
  function detectArch(os, hints) {
    var declared = archFromHints(hints.architecture, hints.bitness);
    if (declared === UNSUPPORTED_32) {
      return null;
    }
    if (declared) {
      return declared;
    }
    if (os === "macos") {
      return archFromGpu(hints.gpu);
    }
    var fromUserAgent = archFromUserAgent(hints.userAgent);
    if (fromUserAgent) {
      return fromUserAgent;
    }
    return os === "windows" ? "x86_64" : null;
  }

  /*
   * The whole platform decision, pure. `hints` carries `userAgent`,
   * `uaPlatform`, `architecture`, `bitness` and `gpu`, any of which may be
   * missing. Returns `{ os, arch }` with either field possibly null, and a
   * null in either field means the caller falls back.
   */
  function detectPlatform(hints) {
    var safe = hints || {};
    var os = detectOs(safe.userAgent, safe.uaPlatform);
    return { os: os, arch: detectArch(os, safe) };
  }

  /* The key both tables are indexed by, or null when the platform is partial. */
  function platformKey(platform) {
    if (!platform || !platform.os || !platform.arch) {
      return null;
    }
    return platform.os + "/" + platform.arch;
  }

  /*
   * The release asset this platform wants, or null when the release carries
   * none it can use. Pure over the asset list the API returned.
   *
   * The candidates are walked in order, so a platform with a second choice
   * takes it only when the first is genuinely absent from this release.
   */
  function pickAsset(platform, assets) {
    var key = platformKey(platform);
    if (!key) {
      return null;
    }
    var candidates = ASSET_CANDIDATES[key];
    if (!candidates || !assets || !assets.length) {
      return null;
    }
    for (var c = 0; c < candidates.length; c += 1) {
      var suffix = candidates[c];
      for (var i = 0; i < assets.length; i += 1) {
        var asset = assets[i];
        var name = asset && asset.name ? String(asset.name) : "";
        if (name.length >= suffix.length && name.slice(name.length - suffix.length) === suffix) {
          return asset;
        }
      }
    }
    return null;
  }

  /* The button label for a platform, or null when there is no confident one. */
  function labelFor(platform) {
    var key = platformKey(platform);
    return key ? LABELS[key] || null : null;
  }

  /*
   * The architecture of the asset that was actually CHOSEN, from its own file
   * name (DRAGON-707).
   *
   * Deriving it from the asset rather than from `detectArch` is the whole
   * point. The two agree on five of the six pairs and disagree on the sixth:
   * an ARM Windows machine is offered the x86_64 msi whenever the release
   * carries no aarch64 one, so a detection-derived parameter would report an
   * aarch64 download that never happened. The offered asset is the truth.
   *
   * Every artifact this project ships names its architecture in the token
   * immediately before the extension: "...-0.33.0-x86_64.msi",
   * "CosmicCaptureKit-aarch64.AppImage". Requiring that position, rather than
   * hunting the token anywhere in the name, is what stops a future release
   * folder or a version that happened to contain those characters from
   * answering. A name that does not carry one, which is what the pre-0.29
   * arch-less releases look like, returns null and the parameter is omitted.
   */
  function archFromAsset(name) {
    var match = /-(x86_64|aarch64)\.[^.]+$/.exec(String(name || ""));
    return match ? match[1] : null;
  }

  /*
   * The artifact kind each extension names, and the ONLY four that count as
   * one. It is an allowlist, not a formatter: an unrecognised extension has to
   * produce null so the parameter is omitted, because an event that invents a
   * kind is worse than an event missing one.
   *
   * It also fixes the spelling, which is why "appimage" is not simply the
   * extension lower cased at the call site: the file is named ".AppImage" and
   * the event must read the same as the other three no matter how the release
   * spells it.
   *
   * The zip is listed although no platform is offered one today. That matches
   * the candidate table above, which lists pairs no asset exists for so that
   * the day one ships it works with no code change here.
   */
  var ASSET_KINDS = {
    dmg: "dmg",
    msi: "msi",
    appimage: "appimage",
    zip: "zip"
  };

  /* The kind of a chosen asset, from its extension, or null. */
  function kindFromAsset(name) {
    var text = String(name || "");
    var dot = text.lastIndexOf(".");
    if (dot === -1) {
      return null;
    }
    var ext = text.slice(dot + 1).toLowerCase();
    /*
     * `hasOwnProperty` and not a bare lookup, because unlike every other table
     * here this key comes from the API's data rather than from a string written
     * in this file. An asset named "x.constructor" would otherwise read a
     * function off the prototype chain and hand it to the event as a kind.
     */
    if (!Object.prototype.hasOwnProperty.call(ASSET_KINDS, ext)) {
      return null;
    }
    return ASSET_KINDS[ext];
  }

  /*
   * The version, from the RELEASE's own `tag_name`, which is the one place it
   * is always written down. The msi and dmg names carry it too, but the
   * AppImages carry no version at all by design, so reading names would leave
   * Linux, the platform steered hardest to a single artifact, as the one
   * platform whose downloads could not be counted per release.
   *
   * The leading "v" comes off, so the parameter reads "0.33.0" and matches the
   * version the app itself reports, and it comes off ONLY when a digit follows
   * it. Any other tag shape is passed through whole rather than quietly losing
   * its first letter. A missing or empty tag is null and the parameter is
   * omitted.
   */
  function versionFromTag(tag) {
    var text = String(tag || "").trim();
    if (!text) {
      return null;
    }
    return /^[vV]\d/.test(text) ? text.slice(1) : text;
  }

  /* ---------------------------------------------------------------- */
  /* Analytics                                                         */
  /* ---------------------------------------------------------------- */

  /*
   * Send one GA4 event, or do nothing at all.
   *
   * Material's Google Analytics integration does NOT publish a global `gtag`
   * function: its snippet keeps that function private and only creates
   * `window.dataLayer`, and it does even that lazily, inside the function the
   * consent banner calls when a visitor presses Accept. So the presence of
   * `window.dataLayer` IS the consent check, and it is the only one needed.
   * Reject, or no answer yet, or an analytics script the network never
   * delivered, and this is a clean no-op.
   *
   * The local shim is not decoration. gtag.js reads the `arguments` object out
   * of the queue, so pushing an ordinary array registers nothing; calling
   * through a function that pushes its own `arguments` reproduces exactly what
   * Google's own snippet pushes.
   */
  function sendEvent(name, params) {
    var layer = window.dataLayer;
    if (!layer || typeof layer.push !== "function") {
      return;
    }
    function gtag() {
      layer.push(arguments);
    }
    gtag("event", name, params || {});
  }

  /*
   * Every event parameter, and the attribute each one is read from. ONE table,
   * so adding a parameter is a row here plus its derivation in `upgrade`, and
   * the writer and the reader can never fall out of step.
   *
   * The order is the order they are written into the event: `file` first,
   * because it is the parameter the other three describe.
   */
  var EVENT_PARAMS = [
    ["file", "data-cck-file"],
    ["arch", "data-cck-arch"],
    ["kind", "data-cck-kind"],
    ["version", "data-cck-version"]
  ];

  /*
   * The parameters a link carries, read straight off its own attributes.
   *
   * An attribute that is absent or empty is OMITTED rather than sent as an
   * empty string, which is what makes "the code could not determine this" a
   * visible fact in the data instead of a value that looks real. This is also
   * what keeps `download_from_releases` parameter free without a special case:
   * its markup carries none of these attributes, so it collects nothing.
   */
  function eventParams(link) {
    var params = {};
    for (var i = 0; i < EVENT_PARAMS.length; i += 1) {
      var value = link.getAttribute(EVENT_PARAMS[i][1]);
      if (value) {
        params[EVENT_PARAMS[i][0]] = value;
      }
    }
    return params;
  }

  /*
   * ONE delegated click listener for every download link on the site, attached
   * to `document` once and never removed.
   *
   * Delegation is what makes this survive Material's instant navigation, which
   * swaps the page content and leaves the document alone: per element
   * listeners would have to be re-attached on every swap, and a missed
   * re-attach is a silently uncounted click. It also means upgrading the
   * button is nothing but an attribute rewrite.
   *
   * The handler never calls `preventDefault` and never waits on anything, so
   * the click proceeds at full speed whether or not analytics is loaded. It is
   * registered passive to say so.
   */
  function installClickTracking() {
    document.addEventListener(
      "click",
      function (event) {
        var target = event.target;
        if (!target || typeof target.closest !== "function") {
          return;
        }
        var link = target.closest("[data-cck-event]");
        if (!link) {
          return;
        }
        var name = link.getAttribute("data-cck-event");
        if (!name) {
          return;
        }
        sendEvent(name, eventParams(link));
      },
      { passive: true }
    );
  }

  /* ---------------------------------------------------------------- */
  /* Effectful parts                                                   */
  /* ---------------------------------------------------------------- */

  /* Read whatever the browser will tell us about the machine. Never throws. */
  function collectHints() {
    var nav = window.navigator || {};
    var hints = {
      userAgent: nav.userAgent || "",
      uaPlatform: null,
      architecture: null,
      bitness: null,
      gpu: null
    };
    var uaData = nav.userAgentData;
    if (!uaData) {
      return Promise.resolve(hints);
    }
    hints.uaPlatform = uaData.platform || null;
    if (typeof uaData.getHighEntropyValues !== "function") {
      return Promise.resolve(hints);
    }
    return uaData.getHighEntropyValues(["architecture", "bitness"]).then(
      function (high) {
        hints.architecture = (high && high.architecture) || null;
        hints.bitness = (high && high.bitness) || null;
        return hints;
      },
      function () {
        /* The browser is allowed to refuse. Unknown, so the caller falls back. */
        return hints;
      }
    );
  }

  /*
   * The GPU's renderer name, for the mac architecture question only. A throw
   * away canvas, created only when the answer can still change the outcome,
   * because a WebGL context is not free and every other platform has already
   * been decided by this point.
   */
  function readGpuRenderer() {
    try {
      var canvas = document.createElement("canvas");
      var gl = canvas.getContext("webgl") || canvas.getContext("experimental-webgl");
      if (!gl) {
        return null;
      }
      var name = "";
      var info = gl.getExtension("WEBGL_debug_renderer_info");
      if (info) {
        name = String(gl.getParameter(info.UNMASKED_RENDERER_WEBGL) || "");
      }
      if (!name) {
        name = String(gl.getParameter(gl.RENDERER) || "");
      }
      return name || null;
    } catch (err) {
      return null;
    }
  }

  /*
   * The latest release, fetched once per page load and shared by every later
   * call. Material's instant navigation keeps this script's state alive across
   * page swaps, so a reader who wanders through the site and comes back to the
   * landing page spends exactly one request, not one per visit.
   *
   * Resolves to null on every failure: no fetch in this browser, a network
   * error, a 403 from the anonymous rate limit, a body that is not JSON, or
   * the timeout above. The caller treats null as "leave the button alone".
   */
  var releasePromise = null;

  function fetchRelease(url) {
    if (releasePromise) {
      return releasePromise;
    }
    if (typeof window.fetch !== "function") {
      releasePromise = Promise.resolve(null);
      return releasePromise;
    }

    var options = { headers: { Accept: "application/vnd.github+json" } };
    var timer = null;
    if (typeof window.AbortController === "function") {
      var controller = new AbortController();
      options.signal = controller.signal;
      timer = window.setTimeout(function () {
        controller.abort();
      }, REQUEST_BUDGET_MS);
    }

    releasePromise = window
      .fetch(url, options)
      .then(function (response) {
        return response && response.ok ? response.json() : null;
      })
      .catch(function () {
        return null;
      })
      .then(function (release) {
        if (timer !== null) {
          window.clearTimeout(timer);
        }
        return release;
      });
    return releasePromise;
  }

  /*
   * Write one derived analytics attribute, or take it off the button entirely.
   *
   * The REMOVAL half is the one that matters. These attributes are written
   * together with the href, so each one either describes the asset now linked
   * or must not exist at all; clearing is how that holds on a button some
   * earlier pass already wrote. It is also the mechanism behind the omission
   * rule, since `eventParams` sends only the attributes that are present.
   */
  function setDerived(el, name, value) {
    if (value) {
      el.setAttribute(name, value);
    } else {
      el.removeAttribute(name);
    }
  }

  /*
   * Turn the fallback button into a direct artifact link. The href, the label,
   * the platform logo and the analytics attributes all move together, so the
   * event that fires can never describe a different link than the one that was
   * pressed, and the logo can never name a platform other than the one the
   * link is for.
   *
   * `arch`, `kind` and `version` are derived HERE, in the same statement run
   * that sets the href, and not at click time (DRAGON-707). At click time the
   * release object is long gone, so `version` would have nowhere to come from,
   * and re-deriving from the file name would be a second copy of rules that
   * could drift from these.
   *
   * The label is written into its own span rather than over the button's own
   * text, because the button also carries the three platform logos and
   * assigning to `button.textContent` would delete them.
   *
   * The logos are the ONLY thing here that is revealed rather than created.
   * The template ships all three hidden, so the icon exists on the page only
   * while this function has resolved a platform: a fallback button, and a
   * page with this script blocked, both keep every one of them hidden.
   */
  function upgrade(button, asset, label, os, tag) {
    button.setAttribute("href", asset.browser_download_url);
    button.setAttribute("data-cck-event", DIRECT_EVENT);
    button.setAttribute("data-cck-file", asset.name);
    setDerived(button, "data-cck-arch", archFromAsset(asset.name));
    setDerived(button, "data-cck-kind", kindFromAsset(asset.name));
    setDerived(button, "data-cck-version", versionFromTag(tag));

    var text = button.querySelector("[data-cck-label]");
    if (text) {
      text.textContent = label;
    } else {
      button.textContent = label;
    }

    var icons = button.querySelectorAll("[data-cck-os]");
    for (var i = 0; i < icons.length; i += 1) {
      icons[i].hidden = icons[i].getAttribute("data-cck-os") !== os;
    }
  }

  /*
   * Run once per document. Returns immediately on every documentation page,
   * which has no such button, and on the landing page only ever moves the
   * button from its correct fallback state to a more specific correct state.
   */
  function enhance() {
    var button = document.querySelector("[data-cck-download]");
    if (!button) {
      return;
    }
    var api = button.getAttribute("data-cck-releases-api");
    if (!api) {
      return;
    }

    collectHints()
      .then(function (hints) {
        var platform = detectPlatform(hints);
        if (platform.os === "macos" && !platform.arch) {
          hints.gpu = readGpuRenderer();
          platform = detectPlatform(hints);
        }
        var label = labelFor(platform);
        if (!label) {
          return null;
        }
        return fetchRelease(api).then(function (release) {
          var asset = pickAsset(platform, release && release.assets);
          if (!asset || !asset.browser_download_url) {
            return null;
          }
          upgrade(button, asset, label, platform.os, release && release.tag_name);
          return null;
        });
      })
      .catch(function () {
        /* The static button is already the right answer. Say nothing. */
      });
  }

  /* ---------------------------------------------------------------- */
  /* Wiring                                                            */
  /* ---------------------------------------------------------------- */

  if (typeof window !== "undefined" && typeof document !== "undefined") {
    installClickTracking();

    /*
     * `document$` is Material's own document observable, published as a global
     * by the theme bundle, which this file is loaded after. Subscribing to it
     * rather than to `DOMContentLoaded` is what makes the button work when a
     * reader arrives at the landing page through instant navigation, where no
     * new document is ever loaded and no load event ever fires. It replays the
     * current document to a late subscriber, so the ordinary first load is
     * covered by the same line. The other two branches are the honest fallback
     * for a build with the feature or the theme changed out from under us.
     */
    var documents = window.document$;
    if (documents && typeof documents.subscribe === "function") {
      documents.subscribe(enhance);
    } else if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", enhance);
    } else {
      enhance();
    }
  }

  /*
   * The pure decisions, for the node fixture harness. `module` does not exist
   * in a browser, so this is dead weight of exactly one `typeof` check there.
   */
  if (typeof module === "object" && module && module.exports) {
    module.exports = {
      detectOs: detectOs,
      detectPlatform: detectPlatform,
      pickAsset: pickAsset,
      labelFor: labelFor,
      archFromAsset: archFromAsset,
      kindFromAsset: kindFromAsset,
      versionFromTag: versionFromTag
    };
  }
})();
