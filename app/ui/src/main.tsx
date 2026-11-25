import "@fontsource-variable/manrope";
import "./theme/riso.css";
import "./theme/taste.css";
import "./theme/luminous.css";
import "./app.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { initializeTheme } from "./theme/bootstrap";

initializeTheme();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
