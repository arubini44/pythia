-- Debugger scenario: order processing across packages, an object type and
-- a trigger. Lines tagged `-- @name` are found by name from the test.

create table dbgx_customers (
  id           number primary key,
  name         varchar2(40) not null,
  credit_limit number(10,2),
  vip          char(1) default 'N'
);

create table dbgx_orders (
  id          number primary key,
  customer_id number references dbgx_customers,
  order_date  date,
  status      varchar2(10),
  total       number(12,2)
);

create table dbgx_order_lines (
  order_id number references dbgx_orders,
  line_no  number,
  product  varchar2(30),
  qty      number,
  price    number(10,2),
  primary key (order_id, line_no)
);

create table dbgx_audit (
  id       number generated always as identity,
  order_id number,
  msg      varchar2(200)
);

insert into dbgx_customers values (1, 'ACME', 1000, 'Y');
insert into dbgx_customers values (2, 'Globex', 100, 'N');
insert into dbgx_orders values (10, 1, date '2026-10-01', 'NEW', null);
insert into dbgx_orders values (11, 2, date '2026-10-02', 'NEW', null);
insert into dbgx_orders values (12, 1, date '2026-10-03', 'NEW', null);
insert into dbgx_order_lines values (10, 1, 'Widget', 2, 49.95);
insert into dbgx_order_lines values (10, 2, 'Gadget', 3, 19.99);
insert into dbgx_order_lines values (10, 3, 'Cable', 10, 4.5);
insert into dbgx_order_lines values (11, 1, 'Server', 1, 150);
insert into dbgx_order_lines values (12, 1, 'Widget', 5, 49.95);
commit;

create or replace type dbgx_money as object (
  amount   number,
  currency varchar2(3),
  member function plus(p_other in dbgx_money) return dbgx_money,
  member function to_text return varchar2
);
/

create or replace type body dbgx_money as
  member function plus(p_other in dbgx_money) return dbgx_money is
    l_sum number;
  begin
    l_sum := self.amount + p_other.amount; -- @money_plus
    return dbgx_money(l_sum, self.currency);
  end;

  member function to_text return varchar2 is
  begin
    return to_char(amount, 'FM999990.00') || ' ' || currency;
  end;
end;
/

create or replace type dbgx_codes as table of number;
/

create or replace package dbgx_types as
  type t_line is record (
    line_no pls_integer,
    product varchar2(30),
    qty     number,
    price   number(10,2),
    amount  number
  );
  type t_lines is table of t_line index by pls_integer;
  type t_order is record (
    header   dbgx_orders%rowtype,
    customer dbgx_customers%rowtype,
    lines    t_lines,
    net      dbgx_money,
    tags     sys.odcivarchar2list,
    codes    dbgx_codes
  );
  type t_rates is table of number index by varchar2(30);
  c_vat constant number := 0.081;
  g_trace boolean := true;
  g_note varchar2(100);
  e_credit exception;
  pragma exception_init(e_credit, -20010);
end;
/

create or replace package dbgx_pricing as
  function line_amount(p_line in dbgx_types.t_line) return number;
  function discount(p_customer in dbgx_customers%rowtype, p_total in number) return number;
  function tiered(p_amount in number, p_level in pls_integer := 1) return number;
  procedure apply_tax(p_order in out nocopy dbgx_types.t_order);
end;
/

create or replace package body dbgx_pricing as
  g_calls pls_integer := 0;
  g_rates dbgx_types.t_rates;

  function round_price(p_value in number) return number is
  begin
    return round(p_value, 2); -- @round_price
  end;
  -- @no_code: a breakpoint here moves to the next line with code

  function line_amount(p_line in dbgx_types.t_line) return number is -- @line_amount_entry
    l_amount number;
  begin
    g_calls := g_calls + 1; -- @line_amount_start
    l_amount := p_line.qty * p_line.price; -- @line_amount
    return round_price(l_amount); -- @line_amount_return
  end;

  function discount(p_customer in dbgx_customers%rowtype, p_total in number) return number is
  begin
    return case when p_customer.vip = 'Y' then tiered(p_total) else 0 end; -- @discount
  end;

  function tiered(p_amount in number, p_level in pls_integer := 1) return number is
    l_rate number := g_rates('L' || p_level);
  begin
    if p_level >= 3 or p_amount < 100 then -- @tiered
      return p_amount * l_rate;
    end if;
    return p_amount * l_rate + tiered(p_amount / 2, p_level + 1); -- @tiered_recurse
  end;

  procedure apply_tax(p_order in out nocopy dbgx_types.t_order) is
    l_tax dbgx_money;
  begin
    l_tax := dbgx_money(round_price(p_order.net.amount * dbgx_types.c_vat), p_order.net.currency); -- @apply_tax
    p_order.net := p_order.net.plus(l_tax); -- @apply_tax_plus
    p_order.header.total := p_order.net.amount;
  end;
begin
  g_rates('L1') := 0.05; -- @pricing_init
  g_rates('L2') := 0.02;
  g_rates('L3') := 0.01;
end;
/

create or replace package dbgx_orders_api as
  function load_order(p_id in number) return dbgx_types.t_order;
  procedure process(p_id in number);
  procedure process_all(p_processed out pls_integer);
  procedure spin(p_rounds in pls_integer);
end;
/

create or replace package body dbgx_orders_api as
  g_processed pls_integer := 0;

  procedure check_credit(p_order in dbgx_types.t_order) is
  begin
    if p_order.net.amount > p_order.customer.credit_limit then -- @check_credit
      raise_application_error(-20010, 'Credit limit exceeded for ' || p_order.customer.name); -- @raise
    end if;
  end;

  function load_order(p_id in number) return dbgx_types.t_order is
    l_order dbgx_types.t_order;
    i       pls_integer := 0;
  begin
    select * into l_order.header from dbgx_orders where id = p_id; -- @load_start
    select * into l_order.customer from dbgx_customers where id = l_order.header.customer_id;
    l_order.tags := sys.odcivarchar2list();
    l_order.codes := dbgx_codes();
    for r in (select * from dbgx_order_lines where order_id = p_id order by line_no) loop
      i := i + 1;
      l_order.lines(i).line_no := r.line_no; -- @load_line
      l_order.lines(i).product := r.product;
      l_order.lines(i).qty := r.qty;
      l_order.lines(i).price := r.price;
      l_order.tags.extend;
      l_order.tags(l_order.tags.count) := r.product;
      l_order.codes.extend;
      l_order.codes(l_order.codes.count) := r.qty;
    end loop;
    l_order.net := dbgx_money(0, 'CHF');
    return l_order; -- @load_done
  end;

  procedure process(p_id in number) is
    l_order    dbgx_types.t_order;
    l_total    number := 0;
    l_discount number;
  begin
    l_order := load_order(p_id); -- @process_start
    for i in 1 .. l_order.lines.count loop -- @process_loaded
      l_order.lines(i).amount := dbgx_pricing.line_amount(l_order.lines(i)); -- @process_line
      l_total := l_total + l_order.lines(i).amount; -- @process_sum
    end loop;
    l_discount := dbgx_pricing.discount(l_order.customer, l_total); -- @process_discount
    l_order.net := dbgx_money(l_total - l_discount, 'CHF');
    dbgx_pricing.apply_tax(l_order); -- @process_tax
    check_credit(l_order); -- @process_credit
    update dbgx_orders set status = 'DONE', total = l_order.header.total where id = p_id; -- @process_update
    g_processed := g_processed + 1; -- @process_counted
  exception
    when dbgx_types.e_credit then
      update dbgx_orders set status = 'HOLD' where id = p_id; -- @process_hold
  end;

  procedure process_all(p_processed out pls_integer) is
  begin
    dbgx_types.g_trace := true; -- @process_all_trace
    for o in (select id from dbgx_orders order by id) loop -- @process_all
      process(o.id); -- @process_all_call
    end loop;
    p_processed := g_processed; -- @process_all_done
  end;

  procedure spin(p_rounds in pls_integer) is
    l_round pls_integer := 0;
  begin
    while l_round < p_rounds loop
      l_round := l_round + 1; -- @spin
      dbms_session.sleep(0.05);
    end loop;
  end;
end;
/

create or replace trigger dbgx_orders_bu
before update of status on dbgx_orders
for each row
declare
  l_msg varchar2(200);
begin
  l_msg := :old.status || ' -> ' || :new.status; -- @trigger
  insert into dbgx_audit (order_id, msg) values (:new.id, l_msg); -- @trigger_insert
end;
/
