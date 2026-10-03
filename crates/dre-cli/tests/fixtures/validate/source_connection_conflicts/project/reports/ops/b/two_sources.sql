select * from {{ source('sales', 'orders') }} join {{ source('crm', 'accounts') }} using (id)
