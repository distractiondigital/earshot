import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "@/App";
import { ThemeProvider } from "@/lib/theme";
import { UpdatesProvider } from "@/lib/updates";
import "@/index.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider>
      <UpdatesProvider>
        <App />
      </UpdatesProvider>
    </ThemeProvider>
  </StrictMode>,
);
