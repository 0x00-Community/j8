// Map / Set 插入与查找
const m = new Map();
const s = new Set();
for (let i = 0; i < 100000; i++) { m.set('k' + i, i); s.add(i * 3); }
let hit = 0;
for (let i = 0; i < 200000; i++) { if (s.has(i)) hit += m.get('k' + (i % 100000)); }
hit;
