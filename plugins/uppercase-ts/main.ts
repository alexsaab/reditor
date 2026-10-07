interface EditorContext {
  readonly event: string;
  readonly text: string;
  readonly path: string | null;
  readonly language: string;
}
interface EditorResult { text?: string; message?: string; }
module.exports = (ctx: EditorContext): EditorResult => {
  if (ctx.event !== 'uppercase') return {};
  return { text: ctx.text.toUpperCase() };
};
