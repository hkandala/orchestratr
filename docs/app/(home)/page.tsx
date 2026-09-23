import { redirect } from 'next/navigation';
import { docsRoute } from '@/lib/shared';

export default function HomePage() {
  redirect(docsRoute);
}
