-- 助手消息里各类 markdown 构造的出现次数。
--
-- 为什么存在：crates/md 是有意的子集（子集之外一律转义，不透传原始 HTML）。取舍不该靠印象
-- 维持——助手到底有没有写表格、写图片语法、写任务列表，数一遍就知道。docs/verification.md §15
-- 是第一次的结果，make census / make census-prod 是复跑入口。
--
-- 2026-11-10 改口径：块级与列表那批（引用块、分隔线、setext、嵌套与松散列表、~~~ 围栏、
-- `+` 与 `1)` 两种 marker）**进了子集**，两套渲染器同形（docs/verification.md §46）。它们的行留着
-- 继续数，但括号里的说法变了——现在真要报警的是六行、五类：原始 HTML、图片、任务列表、
-- 引用式链接（含脚注那两种写法）、四格缩进行。
--
-- 只读：必须以 mode=ro 打开，跑在活服务持有的库上。
--   sqlite3 "file:data/kestrel-studio.db?mode=ro" ".read scripts/markdown-census.sql"
--
-- 只用 LIKE 与 char()：线上宿主机的 sqlite 是 3.7，没有 CTE 也没有 instr()。
-- 已支持的两行（表格、删除线）留在里面是为了让各次统计可比，不是为了报警。
select '助手消息(分母)'   as 构造, count(*) as 次数 from messages where role='assistant'
union all select '原始 HTML（会被转义成源码）', sum(content like '%<div%' or content like '%<p %' or content like '%<br%' or content like '%<table%' or content like '%<img%') from messages where role='assistant'
union all select '图片 ![]()（两边都退回文字）',  sum(content like '%![%')   from messages where role='assistant'
union all select '任务列表 - [ ]（两边都退回文字）', sum(content like '%- [ ]%' or content like '%- [x]%') from messages where role='assistant'
union all select '波浪线围栏 ~~~（已支持）',      sum(content like '%~~~%')  from messages where role='assistant'
union all select '脚注 [^（退回文字）',          sum(content like '%[^%')   from messages where role='assistant'
union all select '引用式链接 ][（退回文字）',     sum(content like '%][%')   from messages where role='assistant'
-- 四格缩进：CommonMark 与 marked 把它当**缩进代码块**，子集里没有这条语法——围栏才是这个产品的
-- 代码形状，所以两边现在都把它当文字（Nest 侧由 renderer.code 退回来）。这一行留着是为了看见
-- 正文开始缩进：那时「缩进代码块要不要进子集」要重新谈，而不是现在。
union all select '四格缩进行（子集外，两边都当文字）', sum(content like '%'||char(10)||'    %' or content like '    %') from messages where role='assistant'
union all select 'setext 标题（已支持）',         sum(content like '%'||char(10)||'===%') from messages where role='assistant'
-- 下面五类是 2026-11-09 补的：当时它们是 kestrel-md 不认、而两套渲染器实测分歧的地方
-- （docs/verification.md §36），而上面那些行**一条都没数到它们**——原来的哨兵只看「子集外
-- 会不会出现」，看不到「子集外已经出现了却没人报警」。11-10 这五条进了子集，行留着。
union all select '引用块 >（已支持）',            sum(content like '%'||char(10)||'> %' or content like '> %') from messages where role='assistant'
union all select '分隔线 ---（已支持）',          sum(content like '%'||char(10)||'---%' and content not like '%|---%') from messages where role='assistant'
union all select '嵌套列表（已支持）',            sum(content like '%'||char(10)||'  - %' or content like '%'||char(10)||'  * %') from messages where role='assistant'
union all select '有序列表 1.-9.（支持）',       sum(content like '%'||char(10)||'1. %' or content like '%'||char(10)||'2. %' or content like '%'||char(10)||'3. %' or content like '%'||char(10)||'4. %' or content like '%'||char(10)||'5. %' or content like '%'||char(10)||'6. %' or content like '%'||char(10)||'7. %' or content like '%'||char(10)||'8. %' or content like '%'||char(10)||'9. %') from messages where role='assistant'
union all select 'ATX 标题 #（支持）',           sum(content like '%'||char(10)||'#%' or content like '#%') from messages where role='assistant'
union all select 'GFM 表格（已支持）',          sum(content like '%|---%' or content like '%---|%') from messages where role='assistant'
union all select '删除线（已支持）',            sum(content like '%~~%' and content not like '%~~~%') from messages where role='assistant'
union all select '行首列表',                   sum(content like '%'||char(10)||'- %' or content like '%'||char(10)||'* %') from messages where role='assistant'
union all select '粗体',                       sum(content like '%**%')   from messages where role='assistant'
union all select '链接（图片除外）',            sum(content like '%](%' and content not like '%![%') from messages where role='assistant'
union all select '代码围栏',                    sum(content like '%```%')  from messages where role='assistant'
union all select '行内代码（无围栏）',           sum(content like '%`%' and content not like '%```%') from messages where role='assistant'
union all select '最长内容(字符)',              max(length(content))       from messages where role='assistant';
