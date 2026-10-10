export function bind(doc: Document): void {
  const start = doc.getElementById("start");
  start?.addEventListener("click", () => {
    const status = doc.getElementById("upload-status");
    if (status) status.textContent = "Uploading.";
  });
  const file = doc.getElementById("file");
  file?.addEventListener("change", () => {
    const status = doc.getElementById("status");
    if (status) status.textContent = "Ready.";
  });
}
