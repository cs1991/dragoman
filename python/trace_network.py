import asyncio
import json
import urllib.parse
import websockets

async def trace():
    ws_url = "ws://127.0.0.1:9222/devtools/page/F1E9A0908DA749E63F607A76B95A9CDC"
    async with websockets.connect(ws_url, max_size=20*1024*1024) as ws:
        await ws.send(json.dumps({"id": 1, "method": "Network.enable", "params": {}}))
        await ws.send(json.dumps({"id": 2, "method": "Runtime.enable", "params": {}}))

        # 1. Click "4 Argon" in the mode picker
        select_argon_js = """
        (async function() {
            // Find and click the picker button if menu is not already open
            let menuItems = Array.from(document.querySelectorAll('[role="menuitem"], .mat-mdc-menu-item'));
            if (menuItems.length === 0) {
                const pickerBtn = document.querySelector('button[aria-label*="mode picker"], button[aria-label*="model picker"]') ||
                                  Array.from(document.querySelectorAll('button')).find(b => /flash|pro|argon/i.test(b.innerText || ''));
                if (pickerBtn) pickerBtn.click();
                await new Promise(r => setTimeout(r, 500));
                menuItems = Array.from(document.querySelectorAll('[role="menuitem"], .mat-mdc-menu-item'));
            }

            const argonItem = menuItems.find(m => (m.innerText || '').includes('4 Argon'));
            if (argonItem) {
                argonItem.click();
                return { success: true, selected: "4 Argon" };
            }
            return { success: false, items: menuItems.map(m => m.innerText) };
        })();
        """

        await ws.send(json.dumps({
            "id": 10,
            "method": "Runtime.evaluate",
            "params": {"expression": select_argon_js, "returnByValue": True, "awaitPromise": True}
        }))

        # Wait for selection to apply
        await asyncio.sleep(1.5)

        # 2. Submit a test prompt under Argon
        prompt_js = """
        (async function() {
            const inputEl = document.querySelector('rich-textarea div[contenteditable="true"]') ||
                            document.querySelector('div[role="textbox"]') ||
                            document.querySelector('textarea');
            if (!inputEl) throw new Error("No input box");

            inputEl.focus();
            inputEl.innerText = "Network Trace Test for Gemini 4 Argon";
            inputEl.dispatchEvent(new Event('input', { bubbles: true }));
            inputEl.dispatchEvent(new Event('change', { bubbles: true }));
            await new Promise(r => setTimeout(r, 400));

            const sendBtn = document.querySelector('button[aria-label*="Send"], button[jsname*="send"], button.send-button');
            if (sendBtn && !sendBtn.disabled) {
                sendBtn.click();
                return "SENT";
            }
            inputEl.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true }));
            return "SENT_ENTER";
        })();
        """

        await ws.send(json.dumps({
            "id": 20,
            "method": "Runtime.evaluate",
            "params": {"expression": prompt_js, "returnByValue": True, "awaitPromise": True}
        }))

        print("Listening for Network requests...")
        captured_requests = []

        for _ in range(50):
            try:
                msg = await asyncio.wait_for(ws.recv(), timeout=3.0)
                data = json.loads(msg)
                method = data.get("method", "")

                if method == "Network.requestWillBeSent":
                    params = data.get("params", {})
                    req = params.get("request", {})
                    url = req.get("url", "")

                    if "assistant.lamda" in url or "BardChatUi" in url or "batchexecute" in url or "StreamGenerate" in url:
                        post_data = req.get("postData", "")
                        print("\n=======================================================")
                        print(f"[CAPTURED RPC REQUEST]")
                        print(f"URL: {url}")
                        print(f"Method: {req.get('method')}")
                        print("Headers:")
                        for k, v in req.get("headers", {}).items():
                            if k.lower() in ["content-type", "x-goog-batchexecute-bgr", "authorization", "x-same-domain"]:
                                print(f"  {k}: {v}")
                        print(f"Post Data (length {len(post_data)}):")
                        
                        # Parse URL-encoded f.req
                        if "f.req=" in post_data:
                            parsed = urllib.parse.parse_qs(post_data)
                            f_req = parsed.get("f.req", [""])[0]
                            print("\nDecoded f.req:")
                            try:
                                json_req = json.loads(f_req)
                                print(json.dumps(json_req, indent=2)[:2000])
                            except Exception:
                                print(f_req[:2000])
                        else:
                            print(post_data[:1500])
                        print("=======================================================\n")
                        captured_requests.append(url)

                elif method == "Network.responseReceived":
                    params = data.get("params", {})
                    resp = params.get("response", {})
                    url = resp.get("url", "")
                    if "StreamGenerate" in url or "assistant.lamda" in url:
                        print(f"[RESPONSE RECEIVED] Status: {resp.get('status')} {resp.get('statusText')} | URL: {url}")

            except asyncio.TimeoutError:
                if captured_requests:
                    break

asyncio.run(trace())
