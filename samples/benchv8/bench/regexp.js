// 正则匹配与替换
const text = 'The quick brown fox 123 jumps over the lazy dog 4567 ';
let c = 0;
for (let i = 0; i < 20000; i++) {
  if (/\d+/.test(text)) c++;
  c += text.replace(/o/g, '0').length;
}
c;
