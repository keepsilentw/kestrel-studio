-- 助手消息里各类 markdown 构造的出现次数。
--
-- 为什么存在：crates/md 是有意的子集（子集之外一律转义，不透传原始 HTML）。取舍不该靠印象
-- 维持——助手到底有没有写表格、写图片语法、写任务列表，数一遍就知道。docs/verification.md §15
-- 是第一次的结果，make census / make census-prod 是复跑入口。
--
-- 只读：必须以 mode=ro 打开，跑在活服务持有的库上。
--   sqlite3 "file:data/kestrel-studio.db?mode=ro" ".read scripts/markdown-census.sql"
--
-- 只用 LIKE 与 char()：线上宿主机的 sqlite 是 3.7，没有 CTE 也没有 instr()。
-- 已支持的两行（表格、删除线）留在里面是为了让各次统计可比，不是为了报警。
select '助手消息(分母)'   as 构造, count(*) as 次数 from messages where role='assistant'
union all select '原始 HTML（会被转义成源码）', sum(content like '%<div%' or content like '%<p %' or content like '%<br%' or content like '%<table%' or content like '%<img%') from messages where role='assistant'
union all select '图片 ![]()（子集不支持）',    sum(content like '%![%')   from messages where role='assistant'
union all select '任务列表 - [ ]（不支持）',    sum(content like '%- [ ]%' or content like '%- [x]%') from messages where role='assistant'
union all select '波浪线围栏 ~~~（只认 ```）',  sum(content like '%~~~%')  from messages where role='assistant'
union all select '脚注 [^（不支持）',          sum(content like '%[^%')   from messages where role='assistant'
union all select 'setext 标题（只认 ATX）',     sum(content like '%'||char(10)||'===%') from messages where role='assistant'
union all select 'GFM 表格（已支持）',          sum(content like '%|---%' or content like '%---|%') from messages where role='assistant'
union all select '删除线（已支持）',            sum(content like '%~~%' and content not like '%~~~%') from messages where role='assistant'
union all select '行首列表',                   sum(content like '%'||char(10)||'- %' or content like '%'||char(10)||'* %') from messages where role='assistant'
union all select '粗体',                       sum(content like '%**%')   from messages where role='assistant'
union all select '链接（图片除外）',            sum(content like '%](%' and content not like '%![%') from messages where role='assistant'
union all select '代码围栏',                    sum(content like '%```%')  from messages where role='assistant'
union all select '行内代码（无围栏）',           sum(content like '%`%' and content not like '%```%') from messages where role='assistant'
union all select '最长内容(字符)',              max(length(content))       from messages where role='assistant';
