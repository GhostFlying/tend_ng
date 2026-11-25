export function isAnonymousSessionProbe(message) {
  // A new profile is expected to have no Web session before manual login.
  return (
    message.location().url.endsWith("/api/v1/web/session") &&
    /\b401\b/.test(message.text())
  );
}

export function collectPageErrors(page, errors) {
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error" && !isAnonymousSessionProbe(message))
      errors.push(message.text());
  });
}
