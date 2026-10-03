select * from {{ source('crm', 'accounts') }} join {{ source('crm', 'contacts') }} using (id)
