// 数组 push / map / filter / reduce
const a = [];
for (let i = 0; i < 300000; i++) a.push(i);
a.map((x) => x * 3).filter((x) => x % 2 === 0).reduce((s, x) => s + x, 0);
