module.exports = function (ctx) {
  if (ctx.event !== 'header') return {};
  return { text: '// Edited with reditor 🦀\n' + ctx.text };
};
