-- Reporting queries with analytic functions, hierarchies and set operators
SELECT /*+ LEADING(e) USE_HASH(d) */ e.employee_id, e.first_name AS first, e.last_name AS last, d.department_name AS dept,
       ROW_NUMBER() OVER (PARTITION BY e.department_id ORDER BY e.salary DESC, e.hire_date) AS rn,
       SUM(e.salary) OVER (PARTITION BY e.department_id ORDER BY e.hire_date ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) running_total,
       CASE WHEN e.salary > 10000 THEN 'high' WHEN e.salary > 5000 THEN 'medium' ELSE 'low' END AS band
  FROM employees e INNER JOIN departments d ON d.department_id = e.department_id AND d.location_id IN (1700, 1800, 2400)
  LEFT JOIN jobs j USING (job_id)
 WHERE e.hire_date BETWEEN DATE '2015-01-01' AND SYSDATE AND (e.commission_pct IS NULL OR e.commission_pct < 0.2) AND e.manager_id IS NOT NULL
 ORDER BY dept, rn
 FETCH FIRST 50 ROWS ONLY;

select lpad(' ', 2 * (level - 1)) || last_name as tree, level, sys_connect_by_path(last_name, '/') path
from employees
start with manager_id is null
connect by prior employee_id = manager_id
order siblings by last_name;

select department_id from departments
minus
select department_id from employees
intersect
select department_id from departments where location_id = 1700;

select listagg(last_name, ', ') within group (order by last_name) names, count(distinct job_id) jobs, max(salary) keep (dense_rank last order by hire_date) last_sal
from employees
group by department_id
having count(*) > 3 -- only bigger departments
   and avg(salary) > 4000;

select extract(year from hire_date) yr, trim(both ' ' from last_name) ln, cast(salary as number(10, 2)) sal, -salary neg, x.a(+) outer_col
from employees, (select 1 a from dual) x
where employees.employee_id = x.a(+)
  and nvl(commission_pct, 0) = 0;

select *
  from (select e.*, rownum rnum from (select * from employees order by salary desc) e where rownum <= :max_row)
 where rnum > :min_row;

select q'{multi
line ' quote}' txt, 'a''b' s, n'unicode', 1.5e3 num, :bind, &&subst. as subst from dual;
