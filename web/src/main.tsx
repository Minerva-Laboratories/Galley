import { render } from 'preact';
import '@fontsource-variable/inter';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@fontsource-variable/source-serif-4';
import './styles/tokens.css';
import './styles/app.css';
import { App } from './components/App';

render(<App />, document.getElementById('app')!);

// Offline app shell. Only in production builds. The dev server serves fresh modules.
if (import.meta.env.PROD && 'serviceWorker' in navigator) {
  window.addEventListener('load', () => {
    navigator.serviceWorker.register('/sw.js').catch(() => {
      // A failed registration means no offline shell. The app still works online.
    });
  });
}
