// 带比较函数的排序
const a = [];
let seed = 7;
for (let i = 0; i < 100000; i++) { seed = (seed * 1103515245 + 12345) % 2147483648; a.push(seed); }
a.sort((x, y) => x - y);
a[0] + a[a.length - 1];
