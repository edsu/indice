// Sentences for the workroom's scripts, in the page's language. The server
// writes them into <script id="i18n" type="application/json"> (see
// `views::script_strings`), so a script never carries English text of its own.
// Loaded before manage.js and editor.js.

const T = JSON.parse(document.getElementById('i18n').textContent);

// The message `key` with its `{name}` placeholders filled from `args`. A plural
// message is an object with one text per plural category; the form for
// `args.count` comes from the browser's rules for the page's language, with
// `other` (every language has it) when the browser doesn't know the category.
function t(key, args = {}) {
  let text = T[key];
  if (text === undefined) return key;
  if (typeof text === 'object') {
    const lang = document.documentElement.lang;
    const category = new Intl.PluralRules(lang).select(args.count);
    text = text[category] ?? text.other;
    args = { ...args, count: new Intl.NumberFormat(lang).format(args.count) };
  }
  return text.replace(/\{(\w+)\}/g, (placeholder, name) => (name in args ? args[name] : placeholder));
}
