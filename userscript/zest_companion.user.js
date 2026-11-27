// ==UserScript==
// @name         Zest Companion
// @namespace    zest
// @version      0.5
// @description  Signs Zest in to PlayStation and reads your PlayStation purchase dates and SteamGifts wins, through your own signed-in browser. Does nothing on a page Zest did not open.
// @match        https://ca.account.sony.com/api/v1/ssocookie*
// @match        https://store.playstation.com/*
// @match        https://www.steamgifts.com/*
// @grant        GM_xmlhttpRequest
// @grant        GM_setValue
// @grant        GM_getValue
// @grant        GM_deleteValue
// @connect      127.0.0.1
// @connect      localhost
// @updateURL    https://github.com/RobbyRatpoison/zest-library-manager/raw/main/userscript/zest_companion.user.js
// @downloadURL  https://github.com/RobbyRatpoison/zest-library-manager/raw/main/userscript/zest_companion.user.js
// @license      MIT
// ==/UserScript==

// How it works
//   Zest opens a service's page with a fragment: #zest=<port>.<token>.<service>
//   (service: psn-login | psn-dates | steamgifts | steamgifts-full). The script "arms" itself from it and keeps the
//   state with GM_setValue, because sign-in bounces through other sites and a fragment or sessionStorage
//   would be lost on the way. Armed state expires after 15 minutes and is cleared when a run ends.
//   Nothing happens on a page that was not opened by Zest: the script is inert unless armed.
//   All traffic goes to http://127.0.0.1:<port>/zest/... with the token in X-Zest-Token. Secrets (session
//   cookies, the NPSSO) are sent only there, never shown on the page and never logged.

(function () {
    'use strict';

    // ZC-BEGIN (pure helpers; tests/test_zest_companion.py runs this block in node)
    const ARM_TTL_MS = 15 * 60 * 1000;
    const SERVICES = ['psn-login', 'psn-dates', 'steamgifts', 'steamgifts-full'];

    // '#zest=51234.AbC_123-token.psn-login' -> {port, token, service}, or null for anything else.
    function parseArmFragment(hash) {
        const m = /^#?(?:.*&)?zest=(\d{4,5})\.([A-Za-z0-9_-]{16,128})\.([a-z-]+)(?:&.*)?$/.exec(hash || '');
        if (!m) return null;
        const port = Number(m[1]);
        if (port < 1024 || port > 65535 || SERVICES.indexOf(m[3]) < 0) return null;
        return { port, token: m[2], service: m[3] };
    }
    function isFresh(armed, now) {
        return !!armed && typeof armed.at === 'number' && now - armed.at >= 0 && now - armed.at < ARM_TTL_MS;
    }
    // The NPSSO from Sony's ssocookie page text: {"npsso":"...","expires_in":...}. Errors come back as
    // {"error":...} (not signed in), which gives null.
    function npssoFromPage(text) {
        try {
            const j = JSON.parse(text);
            return (j && typeof j.npsso === 'string' && /^[A-Za-z0-9]{20,}$/.test(j.npsso)) ? j.npsso : null;
        } catch (e) { return null; }
    }
    // Why a page that should have held the NPSSO did not: Sony's own error text when it is JSON, else a plain note.
    // Never includes a value from a successful reply.
    function psnPageProblem(text) {
        try {
            const j = JSON.parse(text);
            if (j && typeof j.npsso === 'string') return 'the code was not in the expected form';
            const m = j && (j.error_description || j.message || j.error);
            return typeof m === 'string' && m ? m.slice(0, 100) : 'no sign-in was found';
        } catch (e) { return 'the page was not JSON'; }
    }
    // ZC-END

    // PS-BEGIN (pure helpers; tests/test_userscript_psn.py runs this block in node)
    const PSN_ENDPOINT = 'https://web.np.playstation.com/api/graphql/v1//op';
    // Persisted-query hashes of transactionHistoryRetrieve, newest first. Sony rotates them now and then;
    // the first that the gateway recognises is used.
    const PSN_HASHES = [
        '496f6f30273e14e92498da51447f773b1c9e30c3cfefb8739627c31673d08751',
        'f04fe7c7d8498bee5cd0615400eceb07d77eab097d118475ac9fa0c8446b3a42',
    ];
    function psnHistoryUrl(hash, endDate, limit) {
        const variables = encodeURIComponent(JSON.stringify({
            startDate: '1994-12-03T00:00:00.000Z', endDate, limit }));
        const extensions = encodeURIComponent(JSON.stringify({ persistedQuery: { version: 1, sha256Hash: hash } }));
        return PSN_ENDPOINT + '?operationName=transactionHistoryRetrieve&variables=' + variables +
               '&extensions=' + extensions;
    }
    // The product purchases in one page of transactions: [{id, date, name, sku}]. Wallet top-ups, refunds
    // and subscription charges have no products and are skipped; so is anything missing a date or an id.
    function psnPurchases(transactions) {
        const out = [];
        for (const t of (transactions || [])) {
            const lines = (t && t.purchaseDetails && t.purchaseDetails.productPurchases) || [];
            for (const p of lines) {
                if (!t.date || !p || !p.skuId) continue;
                out.push({ id: String(t.id || '') + ':' + String(p.orderItemId || p.skuId),
                           date: t.date, name: p.productName || '', sku: p.skuId });
            }
        }
        return out;
    }
    // Append `rows` to `all`, skipping any already seen (the cursor can repeat a row on a shared boundary).
    function psnAddUnique(all, seen, rows) {
        let added = 0;
        for (const r of rows) { if (!seen.has(r.id)) { seen.add(r.id); all.push(r); added++; } }
        return added;
    }
    // What a GraphQL reply holds: {block} when it has usable transactions (even alongside partial errors,
    // such as a delisted product with no name), else {error}.
    function psnReadReply(json) {
        const block = json && json.data && json.data.transactionHistoryRetrieve;
        if (block && Array.isArray(block.transactions)) return { block };
        const errs = json && Array.isArray(json.errors)
            ? json.errors.map(e => (e && (e.message || (e.extensions && e.extensions.code))) || 'error').join('; ') : '';
        return { error: errs || 'unexpected reply' };
    }
    // Walk the history page by page. `getPage(hash, endDate, limit)` resolves to the parsed JSON reply.
    // Resolves {rows, pages}; throws an Error with Sony's reason when nothing at all could be read.
    async function psnCollect(getPage, now, onPage) {
        const all = [], seen = new Set();
        let hashIx = 0, limit = 50, endDate = now, pages = 0, lastError = '';
        for (let guard = 0; guard < 400; guard++) {
            const r = psnReadReply(await getPage(PSN_HASHES[hashIx], endDate, limit));
            if (!r.block) {
                lastError = r.error;
                if (all.length) break;                                   // keep what was read
                if (/persisted ?query/i.test(r.error) && hashIx + 1 < PSN_HASHES.length) { hashIx++; continue; }
                if (limit > 15) { limit = 15; continue; }                // some gateways cap the page size
                throw new Error(r.error);
            }
            pages++;
            psnAddUnique(all, seen, psnPurchases(r.block.transactions));
            if (onPage) onPage(all.length, pages);
            if (!r.block.hasMore || !r.block.nextEndDate) break;
            endDate = r.block.nextEndDate;
        }
        return { rows: all, pages, warning: lastError };
    }
    // PS-END

    // ---- arming ------------------------------------------------------------------------------------
    const KEY = 'zest_armed';
    function readArmed() {
        const a = GM_getValue(KEY, null);
        if (isFresh(a, Date.now())) return a;
        if (a) GM_deleteValue(KEY);
        return null;
    }
    const fromUrl = parseArmFragment(location.hash);
    if (fromUrl) {
        GM_setValue(KEY, Object.assign({ at: Date.now() }, fromUrl));
        try { history.replaceState(null, '', location.pathname + location.search); } catch (e) { /* keep the fragment */ }
    }
    const armed = readArmed();
    if (!armed) return;                                   // not opened by Zest: do nothing at all

    // ---- small UI ----------------------------------------------------------------------------------
    // A shadow root, so the site's own CSS cannot restyle it; every property is set explicitly.
    const STYLE = `
        :host { all: initial; }
        .box { position: fixed; z-index: 2147483647; right: 16px; bottom: 16px; width: 340px; max-width: calc(100vw - 32px); box-sizing: border-box;
               padding: 12px 14px; background: #121722; color: #e6ebf5; border: 1px solid #3a475f; border-radius: 8px;
               font: 13px/1.45 system-ui, 'Segoe UI', sans-serif; box-shadow: 0 4px 18px rgba(0,0,0,.5); }
        .t { font-weight: 600; color: #8ab4ff; margin-bottom: 4px; }
        .s { color: #e6ebf5; white-space: pre-wrap; word-break: break-word; }
        .s.ok { color: #7fd08a; } .s.bad { color: #ff9a90; }
        .row { display: flex; gap: 8px; margin-top: 10px; }
        button { font: inherit; padding: 4px 14px; border-radius: 5px; cursor: pointer; border: 1px solid #5d7399;
                 background: #243049; color: #e6ebf5; }
        button:hover { background: #2f3f60; }
        button.go { border-color: #4f8cff; background: #2b5fd0; color: #fff; }`;
    function panel(title) {
        const host = document.createElement('div');
        const root = host.attachShadow({ mode: 'closed' });
        const st = document.createElement('style'); st.textContent = STYLE;
        const box = document.createElement('div'); box.className = 'box';
        const h = document.createElement('div'); h.className = 't'; h.textContent = title;
        const s = document.createElement('div'); s.className = 's';
        const row = document.createElement('div'); row.className = 'row';
        const close = document.createElement('button'); close.textContent = 'Close'; close.onclick = () => host.remove();
        row.appendChild(close);
        box.append(h, s, row); root.append(st, box);
        (document.documentElement).appendChild(host);
        return {
            say(t) { s.textContent = t; s.className = 's'; },
            end(t, ok) { s.textContent = t; s.className = 's ' + (ok ? 'ok' : 'bad'); },
            // An action button next to Close (replaces any earlier one).
            action(label, fn) {
                const old = row.querySelector('.go'); if (old) old.remove();
                const b = document.createElement('button'); b.textContent = label; b.className = 'go'; b.onclick = fn;
                row.insertBefore(b, close);
            },
        };
    }
    function done() { GM_deleteValue(KEY); }

    // ---- talking to Zest ---------------------------------------------------------------------------
    function zest(path, body, timeoutMs) {
        return new Promise((resolve, reject) => {
            GM_xmlhttpRequest({
                method: 'POST', url: 'http://127.0.0.1:' + armed.port + path, timeout: timeoutMs || 60000,
                headers: { 'Content-Type': 'application/json', 'X-Zest-Token': armed.token },
                data: JSON.stringify(body),
                onload: r => { let j = {}; try { j = JSON.parse(r.responseText); } catch (e) { /* below */ } resolve(j); },
                onerror: () => reject(new Error('Could not reach Zest. Make sure it is running, and open the page from Zest again (each run uses a new port).')),
                ontimeout: () => reject(new Error('Zest did not answer in time.')),
            });
        });
    }
    async function send(ui, path, body, okText) {
        ui.say('Sending to Zest...');
        try {
            const r = await zest(path, body);
            if (r.ok) { ui.end(r.message || okText, true); done(); }
            else ui.end('Zest: ' + (r.message || 'it did not accept that.'), false);
        } catch (e) { ui.end(e.message, false); }
    }
    // ---- PlayStation: sign-in ----------------------------------------------------------------------
    // Sony's ssocookie endpoint answers a signed-in browser with the NPSSO as JSON. The page is fetched again
    // rather than read from the document, because some browsers show JSON through a viewer that changes the text.
    async function runPsnLogin() {
        const ui = panel('Zest: connect PlayStation');
        ui.say('Reading your PlayStation sign-in...');
        let text = '';
        try { text = await (await fetch(location.origin + location.pathname, { credentials: 'include', cache: 'no-store' })).text(); }
        catch (e) { ui.end('Could not read the sign-in page.', false); return; }
        const npsso = npssoFromPage(text);
        if (npsso) { await send(ui, '/zest/capture', { service: 'playstation', npsso }, 'Connected. You can close this tab.'); return; }
        const tries = armed.tries || 0, why = psnPageProblem(text);
        if (tries >= 2) {
            ui.end('Sony did not give a sign-in code (' + why + ').\nSign in at playstation.com in this browser, then use Connect in Zest again.', false);
            done(); return;
        }
        GM_setValue(KEY, Object.assign({}, armed, { at: Date.now(), tries: tries + 1 }));
        ui.say('Sony says: ' + why + '.\nSign in to PlayStation on the store page that opens.');
        ui.action('Open sign-in', () => { location.href = 'https://store.playstation.com/'; });
    }
    // On the store: sign in if needed, then continue to the code page.
    function psnStoreHop() {
        const ui = panel('Zest: connect PlayStation');
        ui.say('Sign in to PlayStation in this tab if you are not already signed in, then press Continue.');
        ui.action('Continue', () => { location.href = 'https://ca.account.sony.com/api/v1/ssocookie'; });
    }

    // ---- PlayStation: purchase dates ---------------------------------------------------------------
    async function runPsnDates() {
        const ui = panel('Zest: PlayStation purchase dates');
        ui.say('Reading your transaction history...');
        const getPage = async (hash, endDate, limit) => {
            const rid = (window.crypto && crypto.randomUUID) ? crypto.randomUUID() : String(Date.now()) + Math.random();
            const res = await fetch(psnHistoryUrl(hash, endDate, limit), {
                credentials: 'include',
                headers: {
                    'content-type': 'application/json',
                    'apollographql-client-name': '@sie-ppr-web-checkout/app',
                    'apollographql-client-version': '2.184.0',
                    'x-psn-storefront-type': 'checkout:store',
                    'x-psn-app-ver': '@sie-ppr-web-checkout/app/v2.184.0',
                    'x-psn-request-id': rid,
                },
            });
            let json = null;
            try { json = await res.json(); } catch (e) { /* reported below */ }
            if (!json) throw new Error('Sony answered HTTP ' + res.status + ' without data. Are you signed in to PlayStation?');
            await new Promise(r => setTimeout(r, 120));
            return json;
        };
        let onlineId = '';
        try {
            const r = await fetch('https://io.playstation.com/user/details', { credentials: 'include', headers: { accept: 'application/json' } });
            const j = await r.json();
            onlineId = (j && typeof j.handle === 'string') ? j.handle.replace(/"/g, '') : '';
        } catch (e) { /* optional: Zest refuses another account's purchases when it can compare */ }
        let result;
        try {
            result = await psnCollect(getPage, new Date().toISOString(), (n, pages) => ui.say('Read ' + n + ' purchases (page ' + pages + ')...'));
        } catch (e) { ui.end('Could not read your PlayStation history: ' + (e && e.message ? e.message : e), false); return; }
        if (!result.rows.length) { ui.end('No purchases were found on this account.', false); done(); return; }
        await send(ui, '/zest/purchase-dates', {
            service: 'playstation', online_id: onlineId,
            purchases: result.rows.map(r => ({ date: r.date, name: r.name, sku: r.sku })),
        }, 'Done. You can close this tab.');
    }

    // ---- SteamGifts wins -----------------------------------------------------------------------------
    // SteamGifts is behind Cloudflare, so the pages are read here, in the signed-in tab, and their HTML handed to Zest, which parses it and says whether to read another.
    // Public pass: the account's /user/<name>/giveaways/won pages. Private pass (only while signed in as that account): /giveaways/won, for the "received" state.
    // The private pages hold Steam keys; they go to Zest only, which never reads or stores them. A full refresh (steamgifts-full) also removes wins that are gone.
    async function runSteamGifts(full) {
        const ui = panel(full ? 'Zest: SteamGifts wins (full refresh)' : 'Zest: SteamGifts wins');
        const sleep = ms => new Promise(r => setTimeout(r, ms));
        const fetchPage = async url => { const r = await fetch(url, { credentials: 'include' }); if (!r.ok) throw new Error(url + ' answered HTTP ' + r.status); return r.text(); };
        const av = document.querySelector('a.nav__avatar-outer-wrap[href^="/user/"]');
        const loggedIn = av ? (av.getAttribute('href').replace(/^\/user\//, '').split('/')[0] || '') : '';
        const vm = /^\/user\/([^/]+)\//.exec(location.pathname);
        const viewing = vm ? decodeURIComponent(vm[1]) : '';
        const say = (t) => ui.say(t);
        let begin;
        try { begin = await zest('/zest/sg/session', { logged_in_as: loggedIn, viewing, full_refresh: !!full, pass2: true }); }
        catch (e) { ui.end(e.message, false); return; }
        if (!begin.ok) { ui.end('Zest: ' + (begin.message || 'it did not start the import.'), false); done(); return; }
        const base = location.origin + begin.public_url;
        let page = 1, pages = null;
        while (page <= 400) {
            say('Reading won giveaways... page ' + page + (pages ? ' of ' + pages : ''));
            let html, res;
            try { html = await fetchPage(base + '/search?page=' + page); } catch (e) { ui.end('Could not load page ' + page + ': ' + e.message, false); return; }
            try { res = await zest('/zest/sg/page', { phase: 'public', page, html }); } catch (e) { ui.end(e.message, false); return; }
            if (!res.ok) { ui.end('Zest: ' + (res.message || 'it did not accept the page.'), false); return; }
            pages = res.pages || pages;
            if (!res.more) break;
            page++;
            await sleep(1500);
        }
        if (begin.mode === 'full' && begin.pass2) {
            let plan = { pages: [] };
            try { plan = await zest('/zest/sg/pass2-plan', {}); } catch (e) { /* the public pass alone is still worth applying */ }
            const list = (plan && plan.pages) || [];
            for (let i = 0; i < list.length; i++) {
                say('Checking which gifts you received... (' + (i + 1) + ' of ' + list.length + ')');
                let html;
                try { html = await fetchPage(location.origin + '/giveaways/won/search?page=' + list[i]); } catch (e) { continue; }
                try { await zest('/zest/sg/page', { phase: 'private', page: list[i], html }); } catch (e) { /* keep going */ }
                if (i < list.length - 1) await sleep(1500);
            }
        }
        say('Zest is adding the wins to your library...');
        let fin;
        try { fin = await zest('/zest/sg/finish', {}, 10 * 60 * 1000); } catch (e) { ui.end(e.message, false); return; }
        if (!fin.ok) { ui.end('Zest: ' + (fin.message || 'it could not finish.'), false); done(); return; }
        const sum = fin.summary || {};
        ui.end('Done: ' + (sum.games_in_group || 0) + ' game' + (sum.games_in_group === 1 ? '' : 's') + ' in "Won on SteamGifts"' + (sum.new_wins ? ', ' + sum.new_wins + ' new' : '')
            + (sum.unresolved_refs ? ', ' + sum.unresolved_refs + ' not in your library' : '') + '. You can close this tab.'
            + (begin.mode === 'degraded' ? '\nSigned in as someone else, so received-status and invite-only giveaways were not checked.' : ''), true);
        done();
    }

    // ---- dispatch ----------------------------------------------------------------------------------
    const host = location.hostname;
    if (armed.service === 'psn-login') {
        if (host === 'ca.account.sony.com') runPsnLogin();
        else if (host === 'store.playstation.com') psnStoreHop();
    } else if (armed.service === 'psn-dates' && host === 'store.playstation.com') runPsnDates();
    else if ((armed.service === 'steamgifts' || armed.service === 'steamgifts-full') && host === 'www.steamgifts.com') runSteamGifts(armed.service === 'steamgifts-full');
})();
