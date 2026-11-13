// Adds the iroh-live brand to mdBook's stock page chrome: a wordmark at the
// top of the sidebar that links to the landing page, a "Dark" label for the
// coal theme, and an edit link at the end of each page.
(function () {
    "use strict";

    // The book is served from `<site>/book/`, the landing page from `<site>/`.
    const home = path_to_root + "../";

    const sidebar = document.getElementById("mdbook-sidebar");
    if (sidebar) {
        const brand = document.createElement("a");
        brand.className = "irl-brand";
        brand.href = home;
        // The favicon is the logo, so the sidebar reuses its URL.
        const icon = document.querySelector('link[rel="icon"]');
        brand.innerHTML =
            (icon ? '<img src="' + icon.href + '" alt="" width="26" height="26">' : "") +
            '<span class="irl-wordmark">iroh-<span>live</span></span>' +
            '<span class="irl-tag">docs</span>';
        sidebar.insertBefore(brand, sidebar.firstChild);
    }

    const coal = document.getElementById("mdbook-theme-coal");
    if (coal) coal.textContent = "Dark";

    const main = document.querySelector("#mdbook-content main");
    const edit = document.querySelector('a[rel="edit"]');
    if (main && edit) {
        const footer = document.createElement("footer");
        footer.className = "irl-footer";
        footer.innerHTML =
            '<a href="' + edit.href + '">Edit this page on GitHub</a>' +
            '<a href="' + home + '">iroh-live home</a>';
        main.appendChild(footer);
    }
})();
