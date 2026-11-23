import '@fontsource-variable/manrope';
import '../src/theme/riso.css';
import '../src/app.css';
import '../src/devices/devices.css';
import './gallery.css';
import { createRoot } from 'react-dom/client';
import { Gallery } from './Gallery';

const root = document.getElementById('root');
if (!root) throw new Error('Gallery root missing');
createRoot(root).render(<Gallery />);
