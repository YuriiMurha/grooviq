import '@fontsource-variable/manrope';
import '../src/theme/styles';
import '../src/app.css';
import '../src/devices/devices.css';
import './gallery.css';
import { createRoot } from 'react-dom/client';
import { DEFAULT_THEME } from '../src/theme/preference';
import { Gallery } from './Gallery';

document.documentElement.dataset.theme = DEFAULT_THEME;
const root = document.getElementById('root');
if (!root) throw new Error('Gallery root missing');
createRoot(root).render(<Gallery />);
