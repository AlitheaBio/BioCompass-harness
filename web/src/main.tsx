import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { redeemLocalOwnerLink } from "./localOwner";
import "./styles.css";

const rootEl = document.getElementById("root");
if (rootEl) {
  const root = createRoot(rootEl);
  let navigation = 0;
  function open() {
    const current = ++navigation;
    const openWorkbench = () => {
      if (current === navigation) root.render(<React.StrictMode><App key={current} /></React.StrictMode>);
    };
    void redeemLocalOwnerLink().then(openWorkbench).catch(() => {
      if (current !== navigation) return;
      root.render(<main className="bc-drawer-body">
        <h1>Owner link unavailable</h1>
        <p>Open the latest link in owner-link.txt in the local data directory.</p>
        <button type="button" className="bc-btn" onClick={openWorkbench}>Open workbench</button>
      </main>);
    });
  }
  open();
  window.addEventListener("hashchange", () => {
    if (window.location.hash.startsWith("#local-owner=")) open();
  });
}
