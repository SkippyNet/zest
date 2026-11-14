// Zest website: draws the orange slice, wires up the screenshot tabs and
// asks GitHub for the newest release. No dependencies, no tracking.
(function () {
  "use strict";

  var REPO = "SkippyNet/zest";

  var SEGMENTS = [
    ["Clean", "More than 70 rules for Windows, browsers and apps. Analyze first, tick or untick anything, then clean."],
    ["Registry", "Fifteen kinds of invalid entries, each fix saved to a .reg backup you can restore with one click."],
    ["Privacy", "Switches for what Windows collects and advertises, a cookie keep list, and secure deletion."],
    ["Speed", "Start-up apps, tasks and services with reversible switches. Put heavy background apps to sleep."],
    ["Software", "Uninstall with a sweep for leftovers. Update apps and drivers from Microsoft's own sources."],
    ["Storage", "See what fills a drive, find duplicates and empty folders, wipe free space or a whole drive."],
    ["Backup", "Versioned backups of your folders to browse and restore, plus full system images."],
    ["System", "Hardware inventory with drive and battery health, Windows repair tools, restore points."]
  ];

  // ── The slice ────────────────────────────────────────────────────
  // Geometry matches the app icon: eight segments, the first one pointing up
  // and to the right. Angles are in degrees, clockwise, 0 = east.

  var NS = "http://www.w3.org/2000/svg";
  var SPAN = 360 / SEGMENTS.length;
  var FIRST = -45; // direction of segment 0
  var R = { rim: 100, pithOuter: 87, pithInner: 80, core: 10 };
  var PULL = 15;

  function polar(radius, degrees) {
    var a = (degrees * Math.PI) / 180;
    return [radius * Math.cos(a), radius * Math.sin(a)];
  }

  function fmt(point) {
    return point[0].toFixed(2) + " " + point[1].toFixed(2);
  }

  // A ring sector between two radii and two angles.
  function sector(inner, outer, from, to) {
    return (
      "M" + fmt(polar(inner, from)) +
      "L" + fmt(polar(outer, from)) +
      "A" + outer + " " + outer + " 0 0 1 " + fmt(polar(outer, to)) +
      "L" + fmt(polar(inner, to)) +
      "A" + inner + " " + inner + " 0 0 0 " + fmt(polar(inner, from)) +
      "Z"
    );
  }

  // The juicy part of a segment: inset from its neighbours by the membrane.
  function flesh(from, to) {
    var gap = 2.6; // half the membrane, in user units
    var innerInset = (Math.asin(gap / (R.core + 6)) * 180) / Math.PI;
    var outerInset = (Math.asin(gap / (R.pithInner - 2)) * 180) / Math.PI;
    var inner = R.core + 6;
    var outer = R.pithInner - 2;
    return (
      "M" + fmt(polar(inner, from + innerInset)) +
      "L" + fmt(polar(outer, from + outerInset)) +
      "A" + outer + " " + outer + " 0 0 1 " + fmt(polar(outer, to - outerInset)) +
      "L" + fmt(polar(inner, to - innerInset)) +
      "A" + inner + " " + inner + " 0 0 0 " + fmt(polar(inner, from + innerInset)) +
      "Z"
    );
  }

  function el(name, attrs, parent) {
    var node = document.createElementNS(NS, name);
    for (var key in attrs) node.setAttribute(key, attrs[key]);
    if (parent) parent.appendChild(node);
    return node;
  }

  var host = document.getElementById("slice");
  var title = document.getElementById("segment-title");
  var text = document.getElementById("segment-text");
  var listItems = document.querySelectorAll("#segment-list > div");
  var groups = [];
  var active = -1;

  function select(index) {
    if (index === active) return;
    active = index;
    groups.forEach(function (group, i) {
      var offset = i === index ? polar(PULL, FIRST + i * SPAN) : [0, 0];
      group.style.transform = "translate(" + offset[0].toFixed(2) + "px," + offset[1].toFixed(2) + "px)";
      group.setAttribute("aria-pressed", i === index ? "true" : "false");
    });
    listItems.forEach(function (item, i) {
      item.classList.toggle("active", i === index);
    });
    if (title && text) {
      title.textContent = SEGMENTS[index][0];
      text.textContent = SEGMENTS[index][1];
    }
  }

  if (host) {
    var svg = el("svg", { viewBox: "-122 -122 244 244", role: "presentation" }, host);
    var defs = el("defs", {}, svg);
    var fleshGradient = el("radialGradient", { id: "flesh", gradientUnits: "userSpaceOnUse", cx: 0, cy: 0, r: R.pithInner }, defs);
    el("stop", { offset: "0.1", "stop-color": "#ffd070" }, fleshGradient);
    el("stop", { offset: "1", "stop-color": "#ff921e" }, fleshGradient);
    var rindGradient = el("linearGradient", { id: "rind", gradientUnits: "userSpaceOnUse", x1: -80, y1: -80, x2: 80, y2: 80 }, defs);
    el("stop", { offset: "0", "stop-color": "#ff9e2c" }, rindGradient);
    el("stop", { offset: "1", "stop-color": "#e26a04" }, rindGradient);

    // The pale core stays put when segments move.
    el("circle", { r: R.core + 2.5, fill: "#fff4de" }, svg);

    SEGMENTS.forEach(function (segment, i) {
      var from = FIRST - SPAN / 2 + i * SPAN;
      var to = from + SPAN;
      var group = el("g", { class: "seg", tabindex: "0", role: "button", "aria-label": segment[0] + ". " + segment[1] }, svg);
      el("path", { class: "pith", d: sector(R.core, R.pithOuter + 0.5, from, to), fill: "#fff4de" }, group);
      el("path", { class: "rind", d: sector(R.pithOuter, R.rim, from, to), fill: "url(#rind)" }, group);
      el("path", { class: "flesh", d: flesh(from, to), fill: "url(#flesh)" }, group);
      group.addEventListener("mouseenter", function () { select(i); });
      group.addEventListener("focus", function () { select(i); });
      group.addEventListener("click", function () { select(i); });
      group.addEventListener("keydown", function (event) {
        var step = event.key === "ArrowRight" || event.key === "ArrowDown" ? 1
          : event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 0;
        if (step) {
          event.preventDefault();
          groups[(i + step + groups.length) % groups.length].focus();
        }
      });
      groups.push(group);
    });

    listItems.forEach(function (item, i) {
      item.addEventListener("mouseenter", function () { select(i); });
    });

    // The one staged moment: the first segment slides out once the page is up.
    var reduced = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (reduced) select(0);
    else window.setTimeout(function () { select(0); }, 350);
  }

  // ── Screenshot tabs ──────────────────────────────────────────────

  var shot = document.getElementById("shot");
  var tabs = document.querySelectorAll(".tabs button");
  tabs.forEach(function (tab) {
    // A page added in a later version may not have a picture yet.
    var probe = new Image();
    probe.onerror = function () { tab.hidden = true; };
    probe.src = "screenshots/" + tab.dataset.shot + ".png";

    tab.addEventListener("click", function () {
      tabs.forEach(function (other) { other.setAttribute("aria-selected", other === tab ? "true" : "false"); });
      shot.src = "screenshots/" + tab.dataset.shot + ".png";
      shot.alt = tab.dataset.alt;
    });
  });

  // ── Latest release ───────────────────────────────────────────────

  function megabytes(bytes) {
    return (bytes / 1048576).toFixed(1) + " MB";
  }

  function point(id, asset, version, label) {
    var link = document.getElementById(id);
    var meta = document.getElementById(id + "-meta");
    if (!link || !asset) return;
    link.href = asset.browser_download_url;
    if (meta) meta.textContent = label + " " + version + ", " + megabytes(asset.size) + ".";
  }

  if (window.fetch) {
    fetch("https://api.github.com/repos/" + REPO + "/releases/latest", {
      headers: { Accept: "application/vnd.github+json" }
    })
      .then(function (response) { return response.ok ? response.json() : null; })
      .then(function (release) {
        if (!release || !release.tag_name) return;
        var version = release.tag_name.replace(/^v/, "");
        var assets = release.assets || [];
        var setup = assets.filter(function (a) { return /-x64-setup\.exe$/.test(a.name); })[0];
        var portable = assets.filter(function (a) { return /-x64-portable\.zip$/.test(a.name); })[0];
        point("dl-setup", setup, version, "Version");
        point("dl-portable", portable, version, "Version");
        var hero = document.getElementById("hero-download");
        if (hero && setup) {
          hero.href = setup.browser_download_url;
          hero.textContent = "Download Zest " + version;
        }
        var heroVersion = document.getElementById("hero-version");
        if (heroVersion) heroVersion.textContent = "Version " + version + " for Windows 10 and 11, 64-bit.";
        var line = document.getElementById("release-line");
        if (line && release.published_at) {
          var date = new Date(release.published_at).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
          line.textContent = "Version " + version + ", released " + date + ". Windows 10 (1809 or later) and Windows 11, 64-bit.";
        }
      })
      .catch(function () { /* The static links to the release page still work. */ });
  }
})();
