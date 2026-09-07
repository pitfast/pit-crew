import React from "react";
import { createRoot } from "react-dom/client";

function App() {
  return <main data-pitfast="react-static"><h1>PitFast React static example</h1><p>Served by the generic static-web adapter.</p></main>;
}

createRoot(document.getElementById("root")).render(<App />);
