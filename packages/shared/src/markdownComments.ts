/** True when a Markdown body shows nothing: only HTML comments and whitespace. `<!-->` closes itself, and an unclosed comment hides the rest. */
export const rendersEmptyMarkdown = (body: string): boolean =>
  body.replace(/<!--(?:-?>|[\s\S]*?(?:-->|$))/gu, "").trim().length === 0;
