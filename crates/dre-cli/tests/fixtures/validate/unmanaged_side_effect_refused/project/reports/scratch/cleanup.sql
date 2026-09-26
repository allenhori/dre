select 1 as n;
-- tidy up
delete from accounts where closed;
create table keep as select 1;
