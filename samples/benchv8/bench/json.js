// JSON serialization and parsing
const data = [];
for (let i = 0; i < 2000; i++) data.push({ id: i, name: 'item' + i, tags: ['a', 'b', 'c'], nested: { v: i / 3 } });
let total = 0;
for (let r = 0; r < 20; r++) total += JSON.parse(JSON.stringify(data)).length;
total;
