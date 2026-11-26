// ==UserScript==
// @name         Zest Companion
// @namespace    zest
// @version      0.4
// @description  Lets Zest read your Humble Bundle, IndieGala, Nintendo, PlayStation and SteamGifts accounts, and sign in to itch.io, Xbox, Amazon Games and GOG, through your own signed-in browser
// @match        https://www.humblebundle.com/*
// @match        https://accounts.nintendo.com/*
// @match        https://ca.account.sony.com/api/v1/ssocookie*
// @match        https://store.playstation.com/*
// @match        https://www.indiegala.com/*
// @match        https://www.steamgifts.com/*
// @match        https://help.steampowered.com/*
// @match        https://itch.io/*
// @match        https://login.live.com/*
// @match        https://amazon.com/*
// @match        https://www.amazon.com/*
// @match        https://auth.gog.com/*
// @match        https://login.gog.com/*
// @match        https://embed.gog.com/*
// @grant        GM_xmlhttpRequest
// @grant        GM_setValue
// @grant        GM_getValue
// @grant        GM_deleteValue
// @connect      127.0.0.1
// @connect      localhost
// @license      MIT
// ==/UserScript==

// How it works
//   Zest opens a service's page with a fragment: #zest=<port>.<token>.<service>
//   (service: humble | indiegala | nintendo | psn-login | psn-dates | itch | xbox | amazon | gog | steamgifts | steamgifts-full | steam-dates). The script "arms" itself from it and keeps the
//   state with GM_setValue, because sign-in bounces through other sites and a fragment or sessionStorage
//   would be lost on the way. Armed state expires after 15 minutes and is cleared when a run ends.
//   Nothing happens on a page that was not opened by Zest: the script is inert unless armed.
//   All traffic goes to http://127.0.0.1:<port>/zest/... with the token in X-Zest-Token. Secrets (session
//   cookies, the NPSSO) are sent only there, never shown on the page and never logged.
//   Zest side (not built yet): POST /zest/capture {service, ...}, POST /zest/purchase-dates, GET /zest/hello.

(function () {
    'use strict';

    // ZC-BEGIN (pure helpers; tests/test_zest_companion.py runs this block in node)
    const ARM_TTL_MS = 15 * 60 * 1000;
    const SERVICES = ['humble', 'nintendo', 'psn-login', 'psn-dates', 'indiegala', 'itch', 'xbox', 'amazon', 'gog', 'steamgifts', 'steamgifts-full', 'steam-dates'];

    // '#zest=51234.AbC_123-token.humble' -> {port, token, service}, or null for anything else.
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
    // One Humble order cut down to what Zest's game filter reads (a full order holds every download link and is far larger).
    function trimHumbleOrder(o) {
        const p = o && o.product && typeof o.product === 'object' ? { human_name: o.product.human_name, machine_name: o.product.machine_name } : undefined;
        return {
            created: o && o.created, product: p,
            subproducts: ((o && o.subproducts) || []).map(sp => ({
                machine_name: sp.machine_name, human_name: sp.human_name,
                payee: sp.payee && typeof sp.payee === 'object' ? { machine_name: sp.payee.machine_name, human_name: sp.payee.human_name } : undefined,
                downloads: (sp.downloads || []).map(d => ({ platform: d.platform,
                    download_struct: ((d && d.download_struct) || []).map(x => ({ name: x && x.name })) })),
            })),
        };
    }
    // The three JSON blocks Nintendo embeds in its signed-in page. `read(id)` returns the element's data-json text or null.
    // Null unless the session block carries the idToken Zest needs.
    function nintendoBlocks(read) {
        const get = id => { try { return JSON.parse(read(id)); } catch (e) { return null; } };
        const data = get('data'), state = get('state'), meta = get('meta');
        return (data && typeof data.idToken === 'string' && data.idToken && state) ? { data, state, meta } : null;
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
    // Nintendo's library page is the signed-in one when it embeds the session data (see nintendo.py _load_page).
    function nintendoSignedIn(pathname, hasData) { return /^\/portal\/vgcs/.test(pathname || '') && !!hasData; }
    // Where a sign-in that ends on a page carrying the code lands: Microsoft's blank desktop page (code=), Amazon's home page
    // (openid.oa2.authorization_code=), GOG's on_login_success page (code=). null = not there yet; {ok:true, address} = the full
    // address Zest's sign-in reads the code from; {ok:false, problem} = the provider sent an error instead.
    function landingAddress(service, href) {
        let u; try { u = new URL(href); } catch (e) { return null; }
        const q = k => u.searchParams.get(k) || '';
        let key = '', on = false;
        if (service === 'xbox') { on = u.hostname === 'login.live.com' && u.pathname === '/oauth20_desktop.srf'; key = 'code'; }
        else if (service === 'amazon') { on = /^(www\.)?amazon\.com$/.test(u.hostname); key = 'openid.oa2.authorization_code'; }
        else if (service === 'gog') { on = u.hostname === 'embed.gog.com' && u.pathname === '/on_login_success'; key = 'code'; }
        if (!on) return null;
        if (q(key)) return { ok: true, address: href };
        if (q('error')) return { ok: false, problem: (q('error_description') || q('error')).slice(0, 120) };
        return null;
    }
    // An itch.io API key among the texts of a settings page: 32-64 characters of [A-Za-z0-9_-] (the page also lists
    // shorter labels and dotted or spaced text, which are not keys).
    function itchKeyFrom(values) {
        for (const v of (values || [])) {
            const t = typeof v === 'string' ? v.trim() : '';
            if (t.length >= 32 && t.length <= 64 && /^[A-Za-z0-9_-]+$/.test(t)) return t;
        }
        return null;
    }
    // IndieGala's library page: signed in when it was not sent to /login and carries the library markup; a page holds games when it has a card.
    function indiegalaSignedIn(finalPath, html) { return !/^\/login/.test(finalPath || '') && String(html || '').indexOf('profile-private-page-library') >= 0; }
    function indiegalaHasGames(html) { return String(html || '').indexOf('profile-private-page-library-title-padding-showcase') >= 0; }
    // Steam Help shows an activation date as "Oct 1, 2017" or, for this year, "Mar 25". -> 'YYYY-MM-DD' or null. Month names only, so the machine's time zone cannot move the day.
    const STEAM_MONTHS = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep', 'oct', 'nov', 'dec'];
    function steamDate(str, year) {
        const t = String(str || '').replace(/\u00a0/g, ' ').trim();
        let m = /^([A-Za-z]{3,9})\.?\s+(\d{1,2})(?:,?\s+(\d{4}))?$/.exec(t), mon, day, yr;
        if (m) { mon = m[1]; day = m[2]; yr = m[3]; }
        else if ((m = /^(\d{1,2})\s+([A-Za-z]{3,9})\.?(?:,?\s+(\d{4}))?$/.exec(t))) { day = m[1]; mon = m[2]; yr = m[3]; }
        else return null;
        const mi = STEAM_MONTHS.indexOf(mon.slice(0, 3).toLowerCase());
        const y = yr ? Number(yr) : year, d = Number(day);
        if (mi < 0 || !y || d < 1 || d > new Date(Date.UTC(y, mi + 1, 0)).getUTCDate()) return null;
        return y + '-' + String(mi + 1).padStart(2, '0') + '-' + String(d).padStart(2, '0');
    }
    // The earliest date among a Help page's line-item texts ("Oct 1, 2017 - Added to account ..."), else among its "Activated:" values.
    function earliestSteamDate(rowTexts, activatedTexts, year) {
        const pick = list => (list || []).map(x => steamDate(String(x).replace(/\u00a0/g, ' ').split('-')[0], year)).filter(Boolean).sort()[0] || null;
        return pick(rowTexts) || pick(activatedTexts);
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
    // ---- Humble Bundle -----------------------------------------------------------------------------
    // Read in this signed-in tab: the browser attaches the session cookie itself, so the script never touches it.
    // Zest gets the orders, trimmed, and applies its own game filter.
    async function runHumble() {
        const ui = panel('Zest: read Humble Bundle');
        ui.say('Checking your Humble Bundle sign-in...');
        let list = null;
        try { const r = await fetch('/api/v1/user/order', { credentials: 'include' }); if (r.ok) list = await r.json(); } catch (e) { /* below */ }
        if (!Array.isArray(list)) {
            ui.say('Sign in to Humble Bundle in this tab. Zest continues by itself once you are signed in.');
            if (!/^\/login/.test(location.pathname)) location.href = 'https://www.humblebundle.com/login';
            return;
        }
        const all = list.map(o => o && o.gamekey).filter(k => typeof k === 'string' && k);
        // Zest knows which orders it has already imported (every game of them in the library or removed by you) and says
        // which are worth reading; if it cannot answer, everything is read.
        let keys = all;
        ui.say('Asking Zest which of your ' + all.length + ' orders are new...');
        try { const p = await zest('/zest/plan', { service: 'humble', keys: all }); if (p.ok && Array.isArray(p.read)) keys = p.read.filter(k => typeof k === 'string'); }
        catch (e) { ui.end(e.message, false); return; }
        if (!keys.length) { ui.end('Nothing new: all ' + all.length + ' orders were already imported.', true); done(); return; }
        // Sent in batches, so progress is kept: a closed tab or a failure part-way loses at most the batch in flight, and the
        // next run skips everything already saved (Zest answers /zest/plan from what it holds).
        const BATCH = 25;
        let batch = [], failed = 0, ordersSent = 0, gamesSent = 0;
        const progress = () => 'Saved ' + ordersSent + ' order' + (ordersSent === 1 ? '' : 's') + ' (' + gamesSent + ' games) so far.';
        const flush = async () => {
            if (!batch.length) return true;
            const body = { service: 'humble', data: { orders: batch } };
            if (JSON.stringify(body).length > 14 * 1024 * 1024) { ui.end('A batch was too large to send. ' + progress(), false); return false; }
            let r;
            try { r = await zest('/zest/import', body); } catch (e) { ui.end(e.message + '\n' + progress() + ' Run it again to continue.', false); return false; }
            if (!r.ok) { ui.end('Zest: ' + (r.message || 'it did not accept that.') + '\n' + progress() + ' Run it again to continue.', false); return false; }
            ordersSent += batch.length; gamesSent += (typeof r.count === 'number' ? r.count : 0); batch = [];
            return true;
        };
        for (let i = 0; i < keys.length; i++) {
            ui.say('Reading order ' + (i + 1) + ' of ' + keys.length + ' (' + (all.length - keys.length) + ' already imported, skipped)\n' + progress());
            try {
                const r = await fetch('/api/v1/order/' + encodeURIComponent(keys[i]), { credentials: 'include' });
                if (!r.ok) { failed++; continue; }
                batch.push({ gamekey: keys[i], order: trimHumbleOrder(await r.json()) });
            } catch (e) { failed++; }
            if (batch.length >= BATCH && !(await flush())) return;
            await new Promise(r => setTimeout(r, 150));                 // be gentle: hundreds of orders
        }
        if (!(await flush())) return;
        if (!ordersSent) { ui.end('No orders could be read' + (failed ? ' (' + failed + ' failed)' : '') + '.', false); return; }
        ui.end('Done: ' + progress().replace(' so far', '') + (failed ? ' ' + failed + ' could not be read and were skipped; run it again to retry them.' : ' You can close this tab.'), true);
        done();
    }

    // ---- Nintendo ----------------------------------------------------------------------------------
    // The signed-in Virtual Game Cards page embeds the session data (an idToken) Zest needs to ask Nintendo for the list;
    // the cookie only ever loaded this page. The token goes to Zest and nowhere else, and is never shown.
    async function runNintendo() {
        const ui = panel('Zest: read Nintendo');
        const blocks = location.pathname.indexOf('/portal/vgcs') === 0
            ? nintendoBlocks(id => { const el = document.getElementById(id); return el ? el.getAttribute('data-json') : null; }) : null;
        if (!blocks) {
            ui.say('Sign in to your Nintendo Account in this tab. Zest continues by itself once the game cards page loads.');
            // Nintendo's own login redirects back to the page that asked, so start from the cards page.
            if (!/^\/(portal|login|connect)/.test(location.pathname)) location.href = 'https://accounts.nintendo.com/portal/vgcs';
            return;
        }
        await send(ui, '/zest/import', { service: 'nintendo', data: blocks }, 'Done. You can close this tab.');
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

    // ---- IndieGala ---------------------------------------------------------------------------------
    // The library pages are read in this signed-in tab and sent as they are; Zest parses them with the same code its own sync uses.
    async function runIndiegala() {
        const ui = panel('Zest: read IndieGala');
        ui.say('Checking your IndieGala sign-in...');
        const pages = []; let bytes = 0;
        for (let n = 1; n <= 200; n++) {
            let res, html;
            try { res = await fetch(n === 1 ? '/library' : '/library/showcase/' + n + '/', { credentials: 'include' }); html = await res.text(); }
            catch (e) { ui.end('Could not read page ' + n + ' of your library.', false); return; }
            let path = ''; try { path = new URL(res.url).pathname; } catch (e) { /* treated as signed out */ }
            if (n === 1 && !indiegalaSignedIn(path, html)) {
                ui.say('Sign in to IndieGala in this tab. Zest continues by itself once you are signed in.');
                if (!/^\/login/.test(location.pathname)) location.href = 'https://www.indiegala.com/login';
                return;
            }
            if (!indiegalaHasGames(html)) break;
            pages.push(html); bytes += html.length;
            if (bytes > 14 * 1024 * 1024) { ui.end('Your library is larger than one message to Zest can hold.', false); return; }
            ui.say('Read page ' + n + ' of your library...');
            await new Promise(r => setTimeout(r, 300));
        }
        if (!pages.length) { ui.end('No games were found in your IndieGala library.', false); done(); return; }
        await send(ui, '/zest/import', { service: 'indiegala', data: { pages } }, 'Done. You can close this tab.');
    }

    // ---- itch.io -----------------------------------------------------------------------------------
    // The API keys page shows a key once you generate one or press View; the script waits for it to appear.
    async function runItch() {
        const ui = panel('Zest: get your itch.io API key');
        if (!/^\/user\/settings\/api-keys/.test(location.pathname)) {
            ui.say('Sign in to itch.io in this tab. Zest continues by itself once the API keys page opens.');
            if (!/^\/login/.test(location.pathname)) location.href = 'https://itch.io/user/settings/api-keys';
            return;
        }
        ui.say('Press "Generate new API key" (or "View" next to a key you already have). Zest picks it up from the page.');
        const until = Date.now() + 3 * 60 * 1000;
        while (Date.now() < until) {
            const vals = Array.prototype.map.call(document.querySelectorAll('input,textarea,code,pre,span,td,div'),
                el => (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') ? el.value : (el.childNodes.length === 1 ? el.textContent : ''));
            const key = itchKeyFrom(vals);
            if (key) { await send(ui, '/zest/capture', { service: 'itch', api_key: key }, 'Connected. You can close this tab.'); return; }
            await new Promise(r => setTimeout(r, 1000));
        }
        ui.end('No API key appeared on the page. Use Get API key in Zest again, or paste a key there.', false);
        done();
    }

    // ---- Xbox, Amazon, GOG: sign-in -----------------------------------------------------------------
    // The provider's sign-in ends on a page whose address carries the code; the script sends that address to Zest, whose
    // sign-in exchanges it. Until then it only waits (the sign-in pages themselves are the provider's).
    async function runLanding(label) {
        const ui = panel('Zest: connect ' + label);
        const r = landingAddress(armed.service, location.href);
        if (!r) { ui.say('Sign in to ' + label + ' in this tab. Zest continues by itself once you are signed in.'); return; }
        if (!r.ok) { ui.end(label + ' did not sign you in: ' + r.problem, false); done(); return; }
        await send(ui, '/zest/capture', { service: armed.service, address: r.address }, 'Connected. You can close this tab.');
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

    // ---- Steam activation dates ----------------------------------------------------------------------
    // Zest names the games (POST /zest/steam/plan); each one's Help page is fetched here, in the signed-in tab, and the earliest activation date goes back with /zest/steam/result.
    async function runSteamDates() {
        const ui = panel('Zest: Steam activation dates');
        const sleep = ms => new Promise(r => setTimeout(r, ms));
        let plan;
        try { plan = await zest('/zest/steam/plan', {}); } catch (e) { ui.end(e.message, false); return; }
        if (!plan.ok) { ui.end('Zest: ' + (plan.message || 'it has no games to read.'), false); done(); return; }
        let pageId = '';
        try { if (window.HelpWizard && window.HelpWizard.m_steamid) pageId = String(window.HelpWizard.m_steamid); } catch (e) { /* the check is skipped when the page does not say */ }
        if (pageId && plan.steam_id && pageId !== String(plan.steam_id)) {
            ui.end('Steam is signed in as ' + pageId + ' but Zest is set up for ' + plan.steam_id + '. Nothing was imported.', false); done(); return;
        }
        const items = plan.items || [];
        let saved = 0, missing = 0, idx = 0;
        for (const it of items) {
            idx++;
            ui.say('Reading Steam (' + idx + ' of ' + items.length + ')\n' + (it.name || it.appid));
            let date = null;
            try {
                const res = await fetch('https://help.steampowered.com/en/wizard/HelpWithGame/?appid=' + it.appid, { credentials: 'include' });
                const doc = new DOMParser().parseFromString(await res.text(), 'text/html');
                const rows = Array.prototype.map.call(doc.querySelectorAll('.LineItemRow span:first-child'), el => el.textContent);
                const act = [];
                doc.querySelectorAll('.account_details .help_highlight_text').forEach(el => { if (el.textContent.trim() === 'Activated:' && el.nextElementSibling) act.push(el.nextElementSibling.textContent); });
                date = earliestSteamDate(rows, act, new Date().getFullYear());
            } catch (e) { /* a network error counts as not found */ }
            let r;
            try { r = await zest('/zest/steam/result', { appid: it.appid, date, steam_id: pageId }); } catch (e) { ui.end(e.message, false); return; }
            if (!r.ok) { ui.end('Zest: ' + (r.message || 'it did not accept the answer.'), false); done(); return; }
            if (date) saved++; else missing++;
            if (idx < items.length) await sleep(600);
        }
        ui.end('Done: ' + saved + ' date' + (saved === 1 ? '' : 's') + ' saved, ' + missing + ' not found. You can close this tab.', true);
        done();
    }

    // ---- dispatch ----------------------------------------------------------------------------------
    const host = location.hostname;
    if (armed.service === 'humble' && /humblebundle\.com$/.test(host)) runHumble();
    else if (armed.service === 'nintendo' && host === 'accounts.nintendo.com') runNintendo();
    else if (armed.service === 'psn-login') {
        if (host === 'ca.account.sony.com') runPsnLogin();
        else if (host === 'store.playstation.com') psnStoreHop();
    } else if (armed.service === 'psn-dates' && host === 'store.playstation.com') runPsnDates();
    else if (armed.service === 'indiegala' && host === 'www.indiegala.com') runIndiegala();
    else if (armed.service === 'itch' && host === 'itch.io') runItch();
    else if (armed.service === 'xbox' && host === 'login.live.com') runLanding('Xbox');
    else if (armed.service === 'amazon' && /(^|\.)amazon\.com$/.test(host)) runLanding('Amazon Games');
    else if (armed.service === 'gog' && /\.gog\.com$/.test(host)) runLanding('GOG');
    else if (armed.service === 'steam-dates' && host === 'help.steampowered.com') runSteamDates();
    else if ((armed.service === 'steamgifts' || armed.service === 'steamgifts-full') && host === 'www.steamgifts.com') runSteamGifts(armed.service === 'steamgifts-full');
})();
