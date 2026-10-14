import asyncio
import json
import websockets

async def inspect():
    ws_url = "ws://127.0.0.1:9222/devtools/page/F1E9A0908DA749E63F607A76B95A9CDC"
    async with websockets.connect(ws_url) as ws:
        # Enable Runtime
        await ws.send(json.dumps({"id": 1, "method": "Runtime.enable", "params": {}}))
        await ws.recv()

        # JS inspection script
        js_code = """
        (function() {
            // 1. Account Info
            const accountEl = document.querySelector('a[aria-label*="Google Account"], img[alt*="Google Account"], [data-ogb-account-name]');
            const accountText = accountEl ? (accountEl.getAttribute('aria-label') || accountEl.getAttribute('alt') || '') : '';

            // 2. Model Picker & Dropdowns
            const allButtons = Array.from(document.querySelectorAll('button, [role="button"], [role="combobox"]'));
            const modelCandidates = allButtons.filter(b => {
                const t = (b.innerText || '') + (b.getAttribute('aria-label') || '');
                return /gemini|model|argon|flash|pro|advanced|ultra/i.test(t);
            }).map(b => ({
                text: (b.innerText || '').trim(),
                ariaLabel: b.getAttribute('aria-label'),
                role: b.getAttribute('role'),
                tag: b.tagName
            }));

            // 3. Document Title & URL
            return {
                title: document.title,
                url: window.location.href,
                account: accountText,
                modelCandidates: modelCandidates.slice(0, 15)
            };
        })();
        """

        req = {
            "id": 2,
            "method": "Runtime.evaluate",
            "params": {
                "expression": js_code,
                "returnByValue": True,
                "awaitPromise": True
            }
        }
        await ws.send(json.dumps(req))

        while True:
            msg = await ws.recv()
            data = json.loads(msg)
            if data.get("id") == 2:
                print(json.dumps(data.get("result", {}).get("result", {}).get("value", {}), indent=2))
                break

asyncio.run(inspect())
