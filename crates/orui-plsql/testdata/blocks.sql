CREATE TABLE orders (
  order_id NUMBER(10) GENERATED ALWAYS AS IDENTITY,
  customer_id NUMBER(10) NOT NULL, -- who ordered
  status VARCHAR2(20 CHAR) DEFAULT 'NEW' NOT NULL,
  created_at TIMESTAMP(6) WITH TIME ZONE DEFAULT SYSTIMESTAMP,
  CONSTRAINT orders_pk PRIMARY KEY (order_id),
  CONSTRAINT orders_status_ck CHECK (status IN ('NEW', 'PAID', 'SHIPPED'))
) TABLESPACE users;

create index orders_cust_ix on orders (customer_id, created_at) compress 1;

merge into orders o using (select :id id, :status status from dual) s on (o.order_id = s.id)
when matched then update set o.status = s.status where o.status <> s.status delete where s.status = 'CANCELLED'
when not matched then insert (customer_id, status) values (s.id, s.status);

DECLARE
   TYPE t_orders IS TABLE OF orders%ROWTYPE INDEX BY PLS_INTEGER;
   l_orders t_orders;
   l_total NUMBER := 0;
   l_very_long_variable_name_for_testing VARCHAR2(100);
   CURSOR c_open (p_status VARCHAR2 DEFAULT 'NEW') IS SELECT * FROM orders WHERE status = p_status FOR UPDATE SKIP LOCKED;
   e_bad EXCEPTION;
BEGIN
   <<outer_loop>>
   FOR o IN c_open LOOP
      CASE o.status
         WHEN 'NEW' THEN l_total := l_total + 1;
         WHEN 'PAID' THEN
            BEGIN
               UPDATE orders SET status = 'SHIPPED' WHERE CURRENT OF c_open;
            EXCEPTION WHEN OTHERS THEN NULL;
            END;
         ELSE RAISE e_bad;
      END CASE;
      EXIT outer_loop WHEN l_total > 100;
   END LOOP outer_loop;
   OPEN :rc FOR SELECT order_id, status FROM orders WHERE customer_id = 42 ORDER BY created_at DESC;
   EXECUTE IMMEDIATE 'begin dbms_stats.gather_table_stats(user, :t); end;' USING 'ORDERS';
   l_orders(1).status := 'X'; l_total := l_total * 2 + (l_total - 1) / 3;
   IF l_total > 10 AND l_total < 1000 AND l_very_long_variable_name_for_testing IS NULL AND SYSDATE > DATE '2020-01-01' THEN
      dbms_output.put_line('total: ' || TO_CHAR(l_total, 'FM999G999') || ' / ' || l_very_long_variable_name_for_testing || '!');
   ELSIF l_total = 0 THEN NULL; -- nothing to do
   END IF;
   WHILE l_total > 0 LOOP l_total := l_total - 1; END LOOP;
   /* a block comment
      spanning lines */
   COMMIT;
EXCEPTION
   WHEN e_bad OR value_error THEN
      ROLLBACK;
      RAISE_APPLICATION_ERROR(-20001, 'bad order: ' || SQLERRM);
END;
/
