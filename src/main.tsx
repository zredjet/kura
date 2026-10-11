import './styles/tokens.css';
import './styles/reset.css';
import './styles/global.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './app/App';
import { installAppMenu } from './app/appMenu';
import { syncAppearance } from './app/appearance';

void syncAppearance();
void installAppMenu();

const container = document.getElementById('root');
if (container) {
  createRoot(container).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
