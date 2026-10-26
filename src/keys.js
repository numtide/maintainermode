// Keyboard shortcuts in the style of GitHub. `?` lists them.
//
// A page needs no script of its own: a list row is a `main .Box-row` with
// an `a.Link--primary`, and an element with `data-hotkey="x"` is clicked by
// `x`, the one in the selected row first. Keys typed into a field are never
// read, except Esc and the arrows of the search suggestions.
(function () {
  "use strict";

  var GO = {
    h: ["/", "Home"],
    d: ["/", "Home"],
    r: ["/review", "Review"],
    p: ["/packages", "Packages"],
    s: ["/state", "nixpkgs"],
    n: ["/inbox", "Inbox"],
    i: ["/inbox", "Inbox"],
  };
  var GO_HELP = [
    ["h", "Home"],
    ["r", "Review"],
    ["p", "Packages"],
    ["s", "nixpkgs"],
    ["n", "Inbox"],
  ];

  function visible(el) {
    return !!(el && el.offsetParent);
  }
  function typing(t) {
    return (
      t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))
    );
  }
  function searchBox() {
    return [].find.call(
      document.querySelectorAll("input[data-search]"),
      visible,
    );
  }
  function rows() {
    return [].filter.call(
      document.querySelectorAll("main .Box-row"),
      function (r) {
        return visible(r) && r.querySelector("a.Link--primary");
      },
    );
  }

  var sel = null;
  function select(r) {
    if (sel) sel.classList.remove("key-sel");
    sel = r;
    if (!r) return;
    r.classList.add("key-sel");
    if (!r.hasAttribute("tabindex")) r.setAttribute("tabindex", "-1");
    r.focus({ preventScroll: true });
    r.scrollIntoView({ block: "nearest" });
  }
  function move(by) {
    var L = rows();
    if (!L.length) return false;
    var i = L.indexOf(sel);
    i =
      i < 0
        ? by > 0
          ? 0
          : L.length - 1
        : Math.max(0, Math.min(L.length - 1, i + by));
    select(L[i]);
    return true;
  }

  function hotkeys() {
    return [].filter.call(document.querySelectorAll("[data-hotkey]"), visible);
  }
  function hotkey(k) {
    var all = hotkeys().filter(function (el) {
      return el.getAttribute("data-hotkey") === k;
    });
    var inRow =
      sel &&
      all.find(function (el) {
        return sel.contains(el);
      });
    var free = all.find(function (el) {
      return !el.closest("main .Box-row");
    });
    var el = inRow || free;
    if (!el) return false;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)) el.focus();
    else el.click();
    return true;
  }
  function describe(el) {
    return (
      el.getAttribute("data-hotkey-label") ||
      el.getAttribute("title") ||
      el.getAttribute("aria-label") ||
      el.textContent.trim()
    );
  }

  var help = null;
  function kbd(keys) {
    return keys
      .split(" ")
      .map(function (k) {
        return "<kbd>" + k + "</kbd>";
      })
      .join(" ");
  }
  function esc(s) {
    return s.replace(/[&<>"]/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c];
    });
  }
  function showHelp() {
    if (help && help.open) return help.close();
    var site = [
      ["/ or s", "Search"],
      ["?", "Show this list"],
      ["Esc", "Close, leave the search box or clear the selection"],
    ];
    GO_HELP.forEach(function (g) {
      site.push(["g " + g[0], "Go to " + g[1]]);
    });
    var list = rows().length
      ? [
          ["j", "Select the next row"],
          ["k", "Select the row before"],
          ["o or Enter", "Open the selected row"],
        ]
      : [];
    var seen = {};
    var page = [];
    hotkeys().forEach(function (el) {
      var k = el.getAttribute("data-hotkey");
      if (seen[k]) return;
      seen[k] = 1;
      page.push([k, describe(el)]);
    });
    var html =
      '<form method="dialog" class="keys-head"><h2>Keyboard shortcuts</h2>' +
      '<button class="btn btn-sm" aria-label="Close">Close</button></form>';
    [
      ["Site", site],
      ["Lists", list],
      ["This page", page],
    ].forEach(function (s) {
      if (!s[1].length) return;
      html += "<h3>" + s[0] + "</h3><dl>";
      s[1].forEach(function (r) {
        html +=
          "<dt>" +
          r[0].split(" or ").map(kbd).join(" or ") +
          "</dt><dd>" +
          esc(r[1]) +
          "</dd>";
      });
      html += "</dl>";
    });
    if (!help) {
      help = document.createElement("dialog");
      help.className = "keys-help";
      help.setAttribute("aria-label", "Keyboard shortcuts");
      document.body.appendChild(help);
    }
    help.innerHTML = html;
    help.showModal();
  }

  var g = 0;
  document.addEventListener("keydown", function (e) {
    if (e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return;
    var t = e.target,
      k = e.key;
    if (typing(t)) {
      if (k === "Escape" && t.hasAttribute("data-search")) {
        t.blur();
        e.preventDefault();
      }
      return;
    }
    if (
      k === "Enter" &&
      t &&
      t.closest &&
      t.closest("a,button,summary") &&
      t !== sel
    )
      return;
    if (g && Date.now() - g < 1500 && GO[k]) {
      g = 0;
      location.href = GO[k][0];
    } else if (k === "g") {
      g = Date.now();
    } else if (k === "/" || k === "s") {
      var box = searchBox();
      if (box) {
        box.focus();
        box.select();
      } else location.href = "/search";
    } else if (k === "?") {
      showHelp();
    } else if (k === "j") {
      if (!move(1)) return;
    } else if (k === "k") {
      if (!move(-1)) return;
    } else if ((k === "o" || k === "Enter") && sel) {
      var a = sel.querySelector("a.Link--primary");
      if (!a) return;
      location.href = a.href;
    } else if (k === "Escape" && sel) {
      select(null);
      document.activeElement && document.activeElement.blur();
    } else if (k.length === 1 && hotkey(k)) {
      // done
    } else return;
    if (k !== "g") g = 0;
    e.preventDefault();
  });

  // Suggestions under each search box, as GitHub's "Jump to".
  var KIND = { pkg: "Package", module: "Module", area: "Directory" };
  function suggest(input) {
    var form = input.form,
      list = document.createElement("ul"),
      timer = 0,
      items = [],
      at = -1,
      asked = "";
    list.className = "suggest";
    list.id = "suggest-" + Math.random().toString(36).slice(2);
    list.setAttribute("role", "listbox");
    list.hidden = true;
    form.appendChild(list);
    input.setAttribute("role", "combobox");
    input.setAttribute("aria-autocomplete", "list");
    input.setAttribute("aria-controls", list.id);
    input.setAttribute("aria-expanded", "false");

    function close() {
      list.hidden = true;
      at = -1;
      input.setAttribute("aria-expanded", "false");
      input.removeAttribute("aria-activedescendant");
    }
    function mark(i) {
      at = i;
      [].forEach.call(list.children, function (li, j) {
        li.setAttribute("aria-selected", j === i ? "true" : "false");
      });
      if (i >= 0)
        input.setAttribute("aria-activedescendant", list.children[i].id);
      else input.removeAttribute("aria-activedescendant");
    }
    function render(q, v) {
      items = [];
      (v.results || []).forEach(function (r) {
        items.push([r.name, KIND[r.kind] || "", r.url]);
      });
      (v.people || []).forEach(function (p) {
        items.push(["@" + p.login, "Person", p.url]);
      });
      (v.pull_requests || []).forEach(function (p) {
        items.push(["#" + p.number + " " + p.title, "Pull request", p.url]);
      });
      (v.advisories || []).forEach(function (a) {
        items.push([
          a.id + (a.title ? " " + a.title : ""),
          "Advisory",
          "/search?q=" + encodeURIComponent(a.id),
        ]);
      });
      items.push([
        "All results for “" + q + "”",
        "Search",
        "/search?q=" + encodeURIComponent(q),
      ]);
      list.innerHTML = items
        .map(function (it, i) {
          return (
            '<li role="option" id="' +
            list.id +
            "-" +
            i +
            '"><a href="' +
            esc(it[2]) +
            '" tabindex="-1">' +
            '<span class="suggest-name">' +
            esc(it[0]) +
            '</span><span class="suggest-kind">' +
            esc(it[1]) +
            "</span></a></li>"
          );
        })
        .join("");
      list.hidden = false;
      input.setAttribute("aria-expanded", "true");
      mark(-1);
    }
    function ask() {
      var q = input.value.trim();
      if (q.length < 2) return close();
      if (q === asked) return;
      asked = q;
      fetch("/api/v1/search?limit=5&q=" + encodeURIComponent(q), {
        headers: { Accept: "application/json" },
      })
        .then(function (r) {
          return r.ok ? r.json() : null;
        })
        .then(function (v) {
          if (v && input.value.trim() === q && document.activeElement === input)
            render(q, v);
        })
        .catch(function () {});
    }
    input.addEventListener("input", function () {
      clearTimeout(timer);
      asked = "";
      timer = setTimeout(ask, 150);
    });
    input.addEventListener("focus", function () {
      if (input.value.trim().length >= 2) ask();
    });
    // Without this, a press held longer than a tap blurs the input, the list
    // closes, and the click never reaches the link.
    list.addEventListener("mousedown", function (e) {
      e.preventDefault();
    });
    input.addEventListener("blur", function () {
      close();
      asked = "";
    });
    input.addEventListener("keydown", function (e) {
      if (list.hidden) return;
      var n = list.children.length;
      if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n"))
        mark((at + 1) % n);
      else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p"))
        mark(at <= 0 ? n - 1 : at - 1);
      else if (e.key === "Enter" && at >= 0) location.href = items[at][2];
      else if (e.key === "Escape") close();
      else return;
      e.preventDefault();
      e.stopPropagation();
    });
  }
  [].forEach.call(document.querySelectorAll("input[data-search]"), suggest);

  // A page sends what it knows at once; a slow source, such as Hydra,
  // comes after, in place of each `data-part` element.
  [].forEach.call(document.querySelectorAll("[data-part]"), function (el) {
    fetch(el.getAttribute("data-part"))
      .then(function (r) {
        return r.ok ? r.text() : null;
      })
      .then(function (t) {
        if (t !== null) el.outerHTML = t;
      })
      .catch(function () {});
  });
})();
