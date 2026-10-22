create or replace type address_t as object (
  street varchar2(100),
  city   varchar2(50),
  zip    varchar2(10),
  member function format return varchar2,
  constructor function address_t(p_city varchar2) return self as result,
  static procedure validate(p_addr address_t)
) not final;
/
create or replace type body address_t as
  member function format return varchar2 is
  begin
    return street || ', ' || zip || ' ' || city;
  end format;

  constructor function address_t(p_city varchar2) return self as result is
  begin
    self.city := p_city;
    return;
  end;

  static procedure validate(p_addr address_t) is
  begin
    if p_addr.city is null then
      raise_application_error(-20000, 'City required');
    end if;
  end validate;
end;
/
create or replace type address_list_t as table of address_t;
/
