import './styles/tokens.css';
import './styles/reset.css';
import './styles/global.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './app/App';
import { installAppMenu } from './app/appMenu';
import { syncAppearance, watchAppearance } from './app/appearance';

void syncAppearance();
watchAppearance();
void installAppMenu();

// 開発時の自己点検（VITE_KURA_SELFTEST=1。kura:// と CSP。docs/architecture.md §4.2）
if (import.meta.env.VITE_KURA_SELFTEST === '1') {
  void import('./dev/selftest').then((m) => m.startSelftest());
}

const container = document.getElementById('root');
if (container) {
  createRoot(container).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
