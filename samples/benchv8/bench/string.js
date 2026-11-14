// String concatenation, split / join, indexOf
let s = '';
for (let i = 0; i < 50000; i++) s += String.fromCharCode(97 + (i % 26));
let n = 0;
for (let i = 0; i < 200; i++) n += s.split('a').join('-').indexOf('z' + i % 3);
n + s.length;
